//! Web Fetch API compatible `Response` implementation.
//!
//! Native buffered response objects.
//! See https://developer.mozilla.org/en-US/docs/Web/API/Response and
//! https://fetch.spec.whatwg.org/#response-class

use napi::Status;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use crate::fs::{Blob, BlobPropertyBag};

/// Options for constructing a [`Response`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct ResponseInit {
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

/// A Web-compatible `Response` object.
///
/// Implements https://developer.mozilla.org/en-US/docs/Web/API/Response
#[napi]
pub struct Response {
    pub(crate) status: u16,
    pub(crate) status_text: String,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) url: String,
    pub(crate) redirected: bool,
    pub(crate) response_type: String,
    pub(crate) body_used: Arc<AtomicBool>,
}

#[napi]
impl Response {
    /// Construct a new `Response`.
    ///
    /// Accepts an optional body (string or Buffer) and optional `ResponseInit`.
    /// Validates status range (200..=599) and null-body statuses (101, 103, 204, 205, 304).
    #[napi(constructor)]
    pub fn new(
        body: Option<Either<String, Buffer>>,
        init: Option<ResponseInit>,
    ) -> napi::Result<Self> {
        let init = init.unwrap_or_default();
        let status = init.status.unwrap_or(200);

        if !(200..=599).contains(&status) {
            return Err(napi::Error::new(
                Status::InvalidArg,
                format!("status code {status} outside 200..599 range"),
            ));
        }

        let is_null_body_status = matches!(status, 101 | 103 | 204 | 205 | 304);
        if is_null_body_status && body.is_some() {
            return Err(napi::Error::new(
                Status::InvalidArg,
                format!("response with status {status} cannot have a body"),
            ));
        }

        let status_text = match init.status_text {
            Some(st) => {
                if !st
                    .bytes()
                    .all(|b| matches!(b, b'\t' | b' '..=b'~' | 0x80..=0xff))
                {
                    return Err(napi::Error::new(
                        Status::InvalidArg,
                        "statusText contains invalid characters",
                    ));
                }
                st
            }
            None => String::new(),
        };

        let mut headers = HashMap::new();
        if let Some(h) = init.headers {
            for (k, v) in h {
                headers.insert(k.to_lowercase(), v);
            }
        }

        let body_bytes = match body {
            Some(Either::A(s)) => {
                headers
                    .entry("content-type".to_string())
                    .or_insert_with(|| "text/plain;charset=UTF-8".to_string());
                Some(s.into_bytes())
            }
            Some(Either::B(b)) => Some(b.to_vec()),
            None => None,
        };

        Ok(Response {
            status,
            status_text,
            headers,
            body: body_bytes,
            url: String::new(),
            redirected: false,
            response_type: "default".to_string(),
            body_used: Arc::new(AtomicBool::new(false)),
        })
    }

    #[napi(getter)]
    pub fn status(&self) -> u16 {
        self.status
    }

    #[napi(getter)]
    pub fn status_text(&self) -> String {
        self.status_text.clone()
    }

    #[napi(getter)]
    pub fn ok(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    #[napi(getter)]
    pub fn redirected(&self) -> bool {
        self.redirected
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    #[napi(getter, js_name = "type")]
    pub fn response_type(&self) -> String {
        self.response_type.clone()
    }

    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    #[napi(getter)]
    pub fn body_used(&self) -> bool {
        self.body.is_some() && self.body_used.load(Ordering::SeqCst)
    }

    fn consume_body(&self) -> napi::Result<Vec<u8>> {
        if self.body.is_none() {
            return Ok(Vec::new());
        }
        if self.body_used.swap(true, Ordering::SeqCst) {
            return Err(napi::Error::new(
                Status::InvalidArg,
                "TypeError: body already consumed",
            ));
        }
        Ok(self.body.clone().unwrap_or_default())
    }

    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        let bytes = self.consume_body()?;
        // Fetch specification: strip leading UTF-8 BOM if present
        let slice = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            &bytes[3..]
        } else {
            &bytes[..]
        };
        Ok(String::from_utf8_lossy(slice).into_owned())
    }

    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let bytes = self.consume_body()?;
        let slice = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            &bytes[3..]
        } else {
            &bytes[..]
        };
        let s = String::from_utf8_lossy(slice);
        serde_json::from_str(&s).map_err(|e| {
            napi::Error::new(
                Status::InvalidArg,
                format!("SyntaxError: failed to parse json: {e}"),
            )
        })
    }

    #[napi]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        let bytes = self.consume_body()?;
        Ok(bytes.into())
    }

    #[napi]
    pub async fn bytes(&self) -> napi::Result<Buffer> {
        self.array_buffer().await
    }

    #[napi]
    pub fn clone(&self) -> napi::Result<Self> {
        if self.body_used() {
            return Err(napi::Error::new(
                Status::InvalidArg,
                "TypeError: body already consumed",
            ));
        }
        Ok(Response {
            status: self.status,
            status_text: self.status_text.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            url: self.url.clone(),
            redirected: self.redirected,
            response_type: self.response_type.clone(),
            body_used: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Static constructor: `Response.json(data, init)`
    #[napi(factory, js_name = "json")]
    pub fn json_factory(data: serde_json::Value, init: Option<ResponseInit>) -> napi::Result<Self> {
        let json_str = serde_json::to_string(&data).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("json serialize: {e}"))
        })?;
        let mut resp_init = init.unwrap_or_default();
        let mut headers = HashMap::new();
        if let Some(existing) = resp_init.headers.take() {
            for (key, value) in existing {
                headers.insert(key.to_lowercase(), value);
            }
        }
        headers
            .entry("content-type".to_string())
            .or_insert_with(|| "application/json".to_string());
        resp_init.headers = Some(headers);
        Response::new(Some(Either::A(json_str)), Some(resp_init))
    }

    /// Static constructor: `Response.redirect(url, status)`
    #[napi(factory)]
    pub fn redirect(url: String, status: Option<u16>) -> napi::Result<Self> {
        let status = status.unwrap_or(302);
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Err(napi::Error::new(
                Status::InvalidArg,
                format!("invalid redirect status {status}"),
            ));
        }
        let parsed = url::Url::parse(&url).map_err(|e| {
            napi::Error::new(Status::InvalidArg, format!("invalid redirect URL: {e}"))
        })?;
        let mut headers = HashMap::new();
        headers.insert("location".to_string(), parsed.to_string());
        Ok(Response {
            status,
            status_text: String::new(),
            headers,
            body: None,
            url: String::new(),
            redirected: false,
            response_type: "default".to_string(),
            body_used: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Static constructor: `Response.error()`
    #[napi(factory)]
    pub fn error() -> Self {
        Response {
            status: 0,
            status_text: String::new(),
            headers: HashMap::new(),
            body: None,
            url: String::new(),
            redirected: false,
            response_type: "error".to_string(),
            body_used: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `response.body` — a `ReadableStream` of the buffered body, or `null`.
    ///
    /// Each access builds a stream over the same bytes. The body becomes disturbed when the
    /// stream is read, which is what `bodyUsed` and `clone()` observe.
    #[napi(getter)]
    pub fn body<'env>(&self, env: &'env Env) -> napi::Result<Option<ReadableStream<'env, Buffer>>> {
        let Some(bytes) = self.body.clone() else {
            return Ok(None);
        };
        let stream = BodyStream {
            flag: Arc::clone(&self.body_used),
            data: Some(bytes),
        };
        Ok(Some(ReadableStream::new(env, stream)?))
    }

    /// `response.blob()` — `Blob` view of the body, using the `Content-Type` essence.
    #[napi]
    pub async fn blob(&self) -> napi::Result<Blob> {
        let bytes = self.consume_body()?;
        let mime = self
            .headers
            .get("content-type")
            .map(|value| mime_essence(value))
            .unwrap_or_default();
        Ok(Blob::new(
            Some(vec![Either::B(bytes.into())]),
            Some(BlobPropertyBag {
                r#type: Some(mime),
                endings: None,
            }),
        ))
    }

    /// `response.formData()` — parse `application/x-www-form-urlencoded` or `multipart/form-data`.
    ///
    /// Returns a platform `FormData` promise. The body is disturbed even when parsing fails.
    #[napi(ts_return_type = "Promise<FormData>")]
    pub fn form_data(&self, env: Env) -> napi::Result<Unknown<'_>> {
        let bytes = self.consume_body()?;
        let content_type = self.headers.get("content-type").map(String::as_str);
        let entries = parse_form_entries(content_type, &bytes)?;
        let payload = serde_json::Value::Array(entries).to_string();
        let factory: Function<String, Unknown> = env.run_script(FORM_DATA_FACTORY)?;
        factory.call(payload)
    }
}

pub fn response_from_http_response(hr: http_request::HttpResponse, url: String) -> Response {
    let status = hr.get_status_code() as u16;
    let status_text = hr.get_reason_phrase().clone();
    let mut headers = HashMap::new();
    for (k, v) in hr.get_headers().iter() {
        headers.insert(k.to_lowercase(), v.clone());
    }
    let body = hr.get_body().clone();

    Response {
        status,
        status_text,
        headers,
        body: if body.is_empty() && matches!(status, 204 | 205 | 304) {
            None
        } else {
            Some(body)
        },
        url,
        redirected: false,
        response_type: "basic".to_string(),
        body_used: Arc::new(AtomicBool::new(false)),
    }
}

impl Response {
    pub fn set_redirected(&mut self, redirected: bool) {
        self.redirected = redirected;
    }

    pub fn internal_body(&self) -> Option<&[u8]> {
        self.body.as_deref()
    }
}

/// One-shot stream over a buffered body. Reading it marks the response disturbed.
struct BodyStream {
    flag: Arc<AtomicBool>,
    data: Option<Vec<u8>>,
}

impl futures::Stream for BodyStream {
    type Item = napi::Result<Buffer>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.data.is_none() {
            return Poll::Ready(None);
        }
        if this.flag.swap(true, Ordering::SeqCst) {
            this.data = None;
            return Poll::Ready(Some(Err(napi::Error::new(
                Status::InvalidArg,
                "TypeError: body already consumed",
            ))));
        }
        let bytes = this.data.take().unwrap_or_default();
        Poll::Ready(Some(Ok(bytes.into())))
    }
}

const FORM_DATA_FACTORY: &str = r#"(function (payload) {
  const entries = JSON.parse(payload);
  const form = new FormData();
  for (const entry of entries) {
    if (entry.file) {
      const bytes = Buffer.from(entry.data, "base64");
      form.append(
        entry.name,
        new File([bytes], entry.filename || "blob", { type: entry.type || "" }),
      );
    } else {
      form.append(entry.name, entry.value);
    }
  }
  return Promise.resolve(form);
})"#;

fn mime_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn parse_form_entries(
    content_type: Option<&str>,
    bytes: &[u8],
) -> napi::Result<Vec<serde_json::Value>> {
    let Some(content_type) = content_type else {
        return Err(napi::Error::new(
            Status::InvalidArg,
            "TypeError: response formData requires a form content type",
        ));
    };
    let mime = mime_essence(content_type);
    if mime == "application/x-www-form-urlencoded" {
        return Ok(url::form_urlencoded::parse(bytes)
            .into_owned()
            .map(|(name, value)| {
                serde_json::json!({
                    "name": name,
                    "value": value,
                    "file": false,
                })
            })
            .collect());
    }
    if mime == "multipart/form-data" {
        let boundary = multipart_boundary(content_type)?;
        return parse_multipart(bytes, &boundary);
    }
    Err(napi::Error::new(
        Status::InvalidArg,
        "TypeError: response formData content type must be urlencoded or multipart",
    ))
}

fn multipart_boundary(content_type: &str) -> napi::Result<String> {
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("boundary") {
            let boundary = value.trim().trim_matches('"').to_string();
            if boundary.is_empty() {
                break;
            }
            return Ok(boundary);
        }
    }
    Err(napi::Error::new(
        Status::InvalidArg,
        "TypeError: multipart formData is missing a boundary",
    ))
}

fn parse_multipart(body: &[u8], boundary: &str) -> napi::Result<Vec<serde_json::Value>> {
    use base64::engine::general_purpose::STANDARD;
    let marker = format!("--{boundary}").into_bytes();
    let Some(start) = find_slice(body, &marker) else {
        return Ok(Vec::new());
    };
    let mut pos = start + marker.len();
    let mut entries = Vec::new();
    loop {
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        if body.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }
        let Some(relative_header_end) = find_slice(&body[pos..], b"\r\n\r\n") else {
            break;
        };
        let headers = &body[pos..pos + relative_header_end];
        pos += relative_header_end + 4;
        let relative_next = find_slice(&body[pos..], &marker).unwrap_or(body.len() - pos);
        let mut part = &body[pos..pos + relative_next];
        if part.ends_with(b"\r\n") {
            part = &part[..part.len() - 2];
        }
        entries.push(multipart_entry(headers, part, &STANDARD)?);
        pos += relative_next + marker.len();
        if pos > body.len() {
            break;
        }
    }
    Ok(entries)
}

fn multipart_entry(
    headers: &[u8],
    body: &[u8],
    encoder: &impl base64::Engine,
) -> napi::Result<serde_json::Value> {
    let header_text = String::from_utf8_lossy(headers);
    let mut name = None;
    let mut filename = None;
    let mut mime = String::new();
    for line in header_text.split("\r\n") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("content-disposition") {
            for param in value.split(';').skip(1) {
                let Some((param_name, param_value)) = param.split_once('=') else {
                    continue;
                };
                let param_value = param_value.trim().trim_matches('"').to_string();
                match param_name.trim().to_ascii_lowercase().as_str() {
                    "name" => name = Some(param_value),
                    "filename" => filename = Some(param_value),
                    _ => {}
                }
            }
        } else if key.trim().eq_ignore_ascii_case("content-type") {
            mime = value.trim().to_string();
        }
    }
    let Some(name) = name else {
        return Err(napi::Error::new(
            Status::InvalidArg,
            "TypeError: multipart part is missing a name",
        ));
    };
    if let Some(filename) = filename {
        return Ok(serde_json::json!({
            "name": name,
            "file": true,
            "filename": filename,
            "type": mime,
            "data": encoder.encode(body),
        }));
    }
    Ok(serde_json::json!({
        "name": name,
        "file": false,
        "value": String::from_utf8_lossy(body),
    }))
}

fn find_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
