//! Web Fetch API compatible `Request` and `Response` implementations using `http_request`.
//!
//! Native buffered transport objects. The public Web-compatible facade is request.ts.
//! See https://developer.mozilla.org/en-US/docs/Web/API/Request and
//! https://developer.mozilla.org/en-US/docs/Web/API/Response

use napi::Status;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Options for constructing a [`Request`].
#[napi(object)]
#[derive(Default)]
pub struct RequestInit {
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<Either<String, Buffer>>,
    pub mode: Option<String>,
    pub credentials: Option<String>,
    pub cache: Option<String>,
    pub redirect: Option<String>,
    pub referrer: Option<String>,
    pub integrity: Option<String>,
    pub keepalive: Option<bool>,
    pub r#type: Option<String>,
}

pub use crate::net::response::{Response, ResponseInit, response_from_http_response};

/// A Web Fetch API `Request` object.
///
/// Implements https://developer.mozilla.org/en-US/docs/Web/API/Request
#[napi]
pub struct Request {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Option<Vec<u8>>,
    mode: String,
    credentials: String,
    cache: String,
    redirect: String,
    referrer: String,
    integrity: String,
    keepalive: bool,
    body_used: Arc<AtomicBool>,
}

#[napi]
impl Request {
    /// Construct a new `Request`.
    ///
    /// Accepts a URL string or an existing `Request` instance, along with optional `RequestInit`.
    #[napi(constructor)]
    pub fn new(input: Either<String, &Request>, init: Option<RequestInit>) -> napi::Result<Self> {
        let mut init = init.unwrap_or_default();
        if let Either::B(request) = &input {
            if request.body_used() {
                return Err(napi::Error::from_reason("body already consumed"));
            }
            init.mode = init.mode.or_else(|| Some(request.mode.clone()));
            init.credentials = init
                .credentials
                .or_else(|| Some(request.credentials.clone()));
            init.cache = init.cache.or_else(|| Some(request.cache.clone()));
            init.redirect = init.redirect.or_else(|| Some(request.redirect.clone()));
            init.referrer = init.referrer.or_else(|| Some(request.referrer.clone()));
            init.integrity = init.integrity.or_else(|| Some(request.integrity.clone()));
            init.keepalive = init.keepalive.or(Some(request.keepalive));
        }
        let (url, default_method, default_headers, default_body) = match input {
            Either::A(u) => (u, "GET".to_string(), HashMap::new(), None),
            Either::B(r) => (
                r.url.clone(),
                r.method.clone(),
                r.headers.clone(),
                r.body.clone(),
            ),
        };

        let method = init.method.take().unwrap_or(default_method).to_uppercase();

        let mut headers = default_headers;
        if let Some(h) = init.headers.take() {
            for (k, v) in h {
                headers.insert(k.to_lowercase(), v);
            }
        }

        let body = match init.body.take() {
            Some(Either::A(s)) => Some(s.into_bytes()),
            Some(Either::B(b)) => Some(b.to_vec()),
            None => default_body,
        };

        url::Url::parse(&url).map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if (method == "GET" || method == "HEAD") && body.is_some() {
            return Err(napi::Error::from_reason(
                "GET/HEAD requests cannot have a body",
            ));
        }
        Ok(Request {
            url,
            method,
            headers,
            body,
            mode: init.mode.unwrap_or_else(|| "cors".to_string()),
            credentials: init
                .credentials
                .unwrap_or_else(|| "same-origin".to_string()),
            cache: init.cache.unwrap_or_else(|| "default".to_string()),
            redirect: init.redirect.unwrap_or_else(|| "follow".to_string()),
            referrer: init.referrer.unwrap_or_else(|| "about:client".to_string()),
            integrity: init.integrity.unwrap_or_default(),
            keepalive: init.keepalive.unwrap_or(false),
            body_used: Arc::new(AtomicBool::new(false)),
        })
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    #[napi(getter)]
    pub fn method(&self) -> String {
        self.method.clone()
    }

    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    #[napi(getter)]
    pub fn mode(&self) -> String {
        self.mode.clone()
    }

    #[napi(getter)]
    pub fn credentials(&self) -> String {
        self.credentials.clone()
    }

    #[napi(getter)]
    pub fn cache(&self) -> String {
        self.cache.clone()
    }

    #[napi(getter)]
    pub fn redirect(&self) -> String {
        self.redirect.clone()
    }

    #[napi(getter)]
    pub fn referrer(&self) -> String {
        self.referrer.clone()
    }

    #[napi(getter)]
    pub fn integrity(&self) -> String {
        self.integrity.clone()
    }

    #[napi(getter)]
    pub fn keepalive(&self) -> bool {
        self.keepalive
    }

    #[napi(getter)]
    pub fn body_used(&self) -> bool {
        self.body_used.load(Ordering::SeqCst)
    }

    #[napi]
    pub fn clone(&self) -> napi::Result<Self> {
        if self.body_used() {
            return Err(napi::Error::from_reason("body already consumed"));
        }
        Ok(Request {
            url: self.url.clone(),
            method: self.method.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            mode: self.mode.clone(),
            credentials: self.credentials.clone(),
            cache: self.cache.clone(),
            redirect: self.redirect.clone(),
            referrer: self.referrer.clone(),
            integrity: self.integrity.clone(),
            keepalive: self.keepalive,
            body_used: Arc::new(AtomicBool::new(false)),
        })
    }

    fn consume_body(&self) -> napi::Result<Option<Vec<u8>>> {
        if self.body.is_some() && self.body_used.swap(true, Ordering::SeqCst) {
            return Err(napi::Error::from_reason("body already consumed"));
        }
        Ok(self.body.clone())
    }
    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        Ok(String::from_utf8_lossy(&self.consume_body()?.unwrap_or_default()).into_owned())
    }
    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        serde_json::from_str(&self.text().await?)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    }
    #[napi]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        Ok(self.consume_body()?.unwrap_or_default().into())
    }
    #[napi]
    pub async fn bytes(&self) -> napi::Result<Buffer> {
        self.array_buffer().await
    }

    /// Instance method `start` using `http_request` to execute this Request.
    #[napi]
    pub async fn start(&self) -> napi::Result<Response> {
        let body = self.consume_body()?;
        execute_http_request_with_redirect(
            &self.method,
            &self.url,
            &self.headers,
            body.as_deref(),
            &self.redirect,
        )
        .await
    }
}

pub(crate) async fn execute_http_request(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> napi::Result<Response> {
    execute_http_request_with_redirect(method, url, headers, body, "follow").await
}

pub(crate) async fn execute_http_request_with_redirect(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
    redirect_policy: &str,
) -> napi::Result<Response> {
    use http_request::{Body, HttpRequest, Method};
    if !matches!(redirect_policy, "follow" | "manual" | "error") {
        return Err(napi::Error::from_reason("invalid redirect policy"));
    }
    let mut current_url =
        url::Url::parse(url).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let mut current_method = method.to_uppercase();
    if current_method.is_empty()
        || !current_method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err(napi::Error::from_reason("invalid HTTP method"));
    }

    let mut headers = headers.clone();
    let mut body = body.map(<[u8]>::to_vec);
    let mut redirected = false;
    for count in 0..=20 {
        if !matches!(current_url.scheme(), "http" | "https")
            || !current_url.username().is_empty()
            || current_url.password().is_some()
        {
            return Err(napi::Error::from_reason(
                "expected an HTTP(S) URL without credentials",
            ));
        }
        current_url.set_fragment(None);
        let mut request = HttpRequest::get(current_url.as_str());
        request.set_method(match current_method.as_str() {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "PATCH" => Method::Patch,
            "OPTIONS" => Method::Options,
            "CONNECT" | "TRACE" | "TRACK" => {
                return Err(napi::Error::from_reason("forbidden request method"));
            }
            other => Method::Unknown(other.to_owned()),
        });
        for (key, value) in &headers {
            if !matches!(
                key.as_str(),
                "content-length" | "transfer-encoding" | "connection"
            ) {
                request.set_header(key, value);
            }
        }
        request.set_header("connection", "close");
        request.set_header(
            "host",
            current_url[url::Position::BeforeHost..url::Position::AfterPort].to_string(),
        );
        if let Some(bytes) = &body {
            request.set_body(Body::from_bytes(bytes.clone()));
        }
        // Honor platform/SSL_CERT_FILE trust in addition to the bundled roots.
        for cert in rustls_native_certs::load_native_certs().certs {
            request
                .add_root_certificate(cert)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        }
        request.get_mut_config().set_timeout(30_000);
        request.get_mut_config().set_redirect(false);
        let outcome = tokio::task::spawn_blocking(move || request.send())
            .await
            .map_err(|e| napi::Error::from_reason(e.to_string()))?
            .map_err(|e| napi::Error::from_reason(format!("http request failed: {e:?}")))?;
        let mut response = response_from_http_response(outcome, current_url.to_string());
        response.set_redirected(redirected);
        let status = response.status();
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let headers_map = response.headers();
        let Some(location) = headers_map.get("location") else {
            return Ok(response);
        };
        if redirect_policy == "manual" {
            return Ok(response);
        }
        if redirect_policy == "error" {
            return Err(napi::Error::from_reason("redirect error"));
        }
        if count == 20 {
            return Err(napi::Error::from_reason("maximum redirect limit exceeded"));
        }
        let next = current_url
            .join(location)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if next.origin() != current_url.origin() {
            for key in ["authorization", "proxy-authorization", "cookie", "host"] {
                headers.remove(key);
            }
        }
        if ((status == 301 || status == 302) && current_method == "POST")
            || (status == 303 && current_method != "GET" && current_method != "HEAD")
        {
            current_method = "GET".to_owned();
            body = None;
            for key in [
                "content-type",
                "content-length",
                "content-encoding",
                "content-language",
                "content-location",
            ] {
                headers.remove(key);
            }
        }
        current_url = next;
        redirected = true;
    }
    unreachable!()
}

pub type RequestOpts = RequestInit;
