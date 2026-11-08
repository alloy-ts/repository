//! Web Fetch API compatible `Response` implementation.
//!
//! Native buffered response objects.
//! See https://developer.mozilla.org/en-US/docs/Web/API/Response and
//! https://fetch.spec.whatwg.org/#response-class

use napi::Status;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

        let status_text = init.status_text.unwrap_or_default();

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
            return Err(napi::Error::from_reason("body already consumed"));
        }
        Ok(self.body.clone().unwrap_or_default())
    }

    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        let bytes = self.consume_body()?;
        String::from_utf8(bytes)
            .map_err(|e| napi::Error::new(Status::GenericFailure, format!("invalid utf-8: {e}")))
    }

    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let bytes = self.consume_body()?;
        let s = String::from_utf8(bytes)
            .map_err(|e| napi::Error::new(Status::GenericFailure, format!("invalid utf-8: {e}")))?;
        serde_json::from_str(&s).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to parse json: {e}"))
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
            return Err(napi::Error::from_reason("body already consumed"));
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
        let mut headers = resp_init.headers.unwrap_or_default();
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
        url::Url::parse(&url).map_err(|e| {
            napi::Error::new(Status::InvalidArg, format!("invalid redirect URL: {e}"))
        })?;
        let mut headers = HashMap::new();
        headers.insert("location".to_string(), url);
        Ok(Response {
            status,
            status_text: match status {
                301 => "Moved Permanently",
                302 => "Found",
                303 => "See Other",
                307 => "Temporary Redirect",
                308 => "Permanent Redirect",
                _ => "",
            }
            .to_string(),
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
