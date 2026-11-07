//! NAPI bindings for HTTP Request and Response conforming to Web/Mozilla API.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use http_request::{HttpRequest, HttpResponse, Method};
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::error::err_str;

/// RequestInit options dictionary for constructing a [`Request`],
/// conforming to Web/Mozilla Request standards:
/// `{ method?: string, headers?: Record<string, string>, body?: string | Buffer, ... }`
#[napi(object)]
#[derive(Default)]
pub struct RequestInit {
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<Either<String, Buffer>>,
    pub timeout: Option<u32>,
    pub redirect: Option<bool>,
}

/// Request options dictionary when passing a single object containing `url` to `new Request(...)`.
/// `{ url: string, method?: string, headers?: Record<string, string>, body?: string | Buffer, ... }`
#[napi(object)]
#[derive(Default)]
pub struct RequestInitWithUrl {
    pub url: String,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<Either<String, Buffer>>,
    pub timeout: Option<u32>,
    pub redirect: Option<bool>,
}

/// Options dictionary for constructing a [`Response`]: `{ status?: number, statusText?: string, headers?: Record<string, string> }`.
#[napi(object)]
#[derive(Default, Clone)]
pub struct ResponseInit {
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

/// HTTP Response representation conforming to the Web/Mozilla Response interface.
#[napi]
#[derive(Clone)]
pub struct Response {
    status_code: u16,
    status_text: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[napi]
impl Response {
    /// Construct a new Response object conforming to MDN Response:
    /// `new Response(body, init)`
    #[napi(constructor)]
    pub fn new(body: Option<Either<String, Buffer>>, init: Option<ResponseInit>) -> Self {
        let init = init.unwrap_or_default();
        let status_code = init.status.unwrap_or(200);
        let status_text = init.status_text.unwrap_or_else(|| "OK".to_string());
        let headers = init.headers.unwrap_or_default();
        let body_bytes = match body {
            Some(Either::A(s)) => s.into_bytes(),
            Some(Either::B(b)) => b.to_vec(),
            None => Vec::new(),
        };
        Response {
            status_code,
            status_text,
            headers,
            body: body_bytes,
        }
    }

    /// Static `Response.json(data, init)` helper.
    #[napi(factory, js_name = "json")]
    pub fn json_factory(data: serde_json::Value, init: Option<ResponseInit>) -> Result<Self> {
        let bytes = serde_json::to_vec(&data).map_err(|e| err_str("Response.json", e.to_string()))?;
        let mut res = Self::new(Some(Either::B(Buffer::from(bytes))), init);
        res.headers.insert("content-type".to_string(), "application/json".to_string());
        Ok(res)
    }

    /// Static `Response.redirect(url, status)` helper.
    #[napi(factory)]
    pub fn redirect(url: String, status: Option<u16>) -> Self {
        let status_code = status.unwrap_or(302);
        let mut headers = HashMap::new();
        headers.insert("location".to_string(), url);
        Response {
            status_code,
            status_text: "Found".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    pub(crate) fn from_http_response(res: HttpResponse) -> Self {
        let mut headers = HashMap::new();
        for (k, v) in res.headers {
            headers.insert(k, v);
        }
        Response {
            status_code: res.status_code as u16,
            status_text: res.reason_phrase,
            headers,
            body: res.body,
        }
    }

    /// Status code of the response (e.g., 200, 404).
    #[napi(getter)]
    pub fn status(&self) -> u16 {
        self.status_code
    }

    /// Status message corresponding to the status code.
    #[napi(getter)]
    pub fn status_text(&self) -> String {
        self.status_text.clone()
    }

    /// Whether the response was successful (status in the range 200-299).
    #[napi(getter)]
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status_code)
    }

    /// Response headers map.
    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    /// Read response body as text.
    #[napi(ts_return_type = "Promise<string>")]
    pub async fn text(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.body).into_owned())
    }

    /// Read response body as JSON.
    #[napi(ts_return_type = "Promise<any>")]
    pub async fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(|e| err_str("json", e.to_string()))
    }

    /// Read response body as Buffer/Uint8Array bytes.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub async fn array_buffer(&self) -> Result<Buffer> {
        Ok(Buffer::from(self.body.clone()))
    }
}

/// Web-standard `Request` class implemented over `http_request`.
///
/// Can be constructed with:
/// `new Request(url, init)`
/// or
/// `new Request({ url, method, headers, body })`
#[napi]
#[derive(Clone)]
pub struct Request {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Option<Vec<u8>>,
    body_used: Arc<AtomicBool>,
    timeout: Option<u32>,
    redirect: bool,
}

#[napi]
impl Request {
    /// Construct a new `Request` conforming to Mozilla Web Request API.
    ///
    /// Accepts either `(input: string, init?: RequestInit)`
    /// or `(input: { url: string, method?: string, headers?: ..., body?: ... })`.
    #[napi(constructor)]
    pub fn new(
        input: Either<String, RequestInitWithUrl>,
        init: Option<RequestInit>,
    ) -> Result<Self> {
        match input {
            Either::A(url_str) => {
                let init = init.unwrap_or_default();
                let method = init.method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
                let mut headers = HashMap::new();
                if let Some(hdrs) = init.headers {
                    for (k, v) in hdrs {
                        headers.insert(k.to_ascii_lowercase(), v);
                    }
                }
                let body = match init.body {
                    Some(Either::A(text)) => Some(text.into_bytes()),
                    Some(Either::B(buf)) => Some(buf.to_vec()),
                    None => None,
                };
                let redirect = init.redirect.unwrap_or(false);
                Ok(Request {
                    url: url_str,
                    method,
                    headers,
                    body,
                    body_used: Arc::new(AtomicBool::new(false)),
                    timeout: init.timeout,
                    redirect,
                })
            }
            Either::B(obj) => {
                let method = obj.method.unwrap_or_else(|| "GET".to_string()).to_uppercase();
                let mut headers = HashMap::new();
                if let Some(hdrs) = obj.headers {
                    for (k, v) in hdrs {
                        headers.insert(k.to_ascii_lowercase(), v);
                    }
                }
                let body = match obj.body {
                    Some(Either::A(text)) => Some(text.into_bytes()),
                    Some(Either::B(buf)) => Some(buf.to_vec()),
                    None => None,
                };
                let redirect = obj.redirect.unwrap_or(false);
                Ok(Request {
                    url: obj.url,
                    method,
                    headers,
                    body,
                    body_used: Arc::new(AtomicBool::new(false)),
                    timeout: obj.timeout,
                    redirect,
                })
            }
        }
    }

    /// The target URL of the request.
    #[napi(getter)]
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The HTTP method of the request.
    #[napi(getter)]
    pub fn method(&self) -> String {
        self.method.clone()
    }

    /// The headers associated with the request.
    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    /// Whether or not the body has been used in a request yet.
    #[napi(getter)]
    pub fn body_used(&self) -> bool {
        self.body_used.load(Ordering::Relaxed)
    }

    /// Get the request body as text, if present.
    #[napi]
    pub fn text(&self) -> Option<String> {
        self.body_used.store(true, Ordering::Relaxed);
        self.body
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// Get the request body as parsed JSON, if present.
    #[napi]
    pub fn json(&self) -> Result<Option<serde_json::Value>> {
        self.body_used.store(true, Ordering::Relaxed);
        match &self.body {
            Some(b) => serde_json::from_slice(b)
                .map(Some)
                .map_err(|e| err_str("json", e.to_string())),
            None => Ok(None),
        }
    }

    /// Get the request body bytes as Buffer, if present.
    #[napi]
    pub fn array_buffer(&self) -> Option<Buffer> {
        self.body_used.store(true, Ordering::Relaxed);
        self.body.as_ref().map(|b| Buffer::from(b.clone()))
    }

    /// Build an internal `http_request::HttpRequest` from this instance.
    fn to_http_request(&self) -> Result<HttpRequest> {
        let mut req = HttpRequest::get(&self.url);
        let method = Method::from_str(&self.method).unwrap_or(Method::Get);
        req.set_method(method);

        for (k, v) in &self.headers {
            req.set_header(k, v);
        }

        if let Some(b) = &self.body {
            req.set_body(http_request::Body { bytes: b.clone() });
        }

        if let Some(to) = self.timeout {
            req.config.set_timeout(to as u64);
        }
        req.config.set_redirect(self.redirect);

        Ok(req)
    }

    /// Send the request synchronously using `http_request`.
    #[napi]
    pub fn send(&self) -> Result<Response> {
        self.body_used.store(true, Ordering::Relaxed);
        let mut http_req = self.to_http_request()?;
        let res = http_req
            .send()
            .map_err(|e| err_str("Request.send", format!("HTTP request error: {e:?}")))?;
        Ok(Response::from_http_response(res))
    }

    /// Send the request asynchronously using `http_request`.
    #[napi]
    pub async fn send_async(&self) -> Result<Response> {
        self.body_used.store(true, Ordering::Relaxed);
        let mut http_req = self.to_http_request()?;
        let res = tokio::task::spawn_blocking(move || http_req.send())
            .await
            .map_err(|e| err_str("Request.sendAsync", e.to_string()))?
            .map_err(|e| err_str("Request.sendAsync", format!("HTTP request error: {e:?}")))?;
        Ok(Response::from_http_response(res))
    }

    /// Start and run this Request using async send/fetch, returning a Promise resolving to Response.
    #[napi]
    pub async fn start(&self) -> Result<Response> {
        self.send_async().await
    }

    /// Clone this Request instance.
    #[napi]
    pub fn clone(&self) -> Self {
        Clone::clone(self)
    }
}

/// Fetch helper executing a request asynchronously.
#[napi]
pub async fn fetch(
    input: Either<String, RequestInitWithUrl>,
    init: Option<RequestInit>,
) -> Result<Response> {
    let req = Request::new(input, init)?;
    req.send_async().await
}
