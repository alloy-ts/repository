//! Web Fetch API compatible `Request` and `Response` implementations using `http_request`.
//!
//! Confirms to https://developer.mozilla.org/en-US/docs/Web/API/Request and
//! https://developer.mozilla.org/en-US/docs/Web/API/Response

use napi_derive::napi;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use napi::bindgen_prelude::*;
use napi::Status;

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
}

/// Options for constructing a [`Response`].
#[napi(object)]
#[derive(Default, Clone)]
pub struct ResponseInit {
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

/// A Web-compatible `Response` object.
#[napi]
pub struct Response {
    status: u16,
    status_text: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    url: String,
    redirected: bool,
}

#[napi]
impl Response {
    #[napi(constructor)]
    pub fn new(body: Option<Either<String, Buffer>>, init: Option<ResponseInit>) -> Self {
        let init = init.unwrap_or_default();
        let status = init.status.unwrap_or(200);
        let status_text = init.status_text.unwrap_or_else(|| {
            match status {
                200 => "OK",
                201 => "Created",
                204 => "No Content",
                301 => "Moved Permanently",
                302 => "Found",
                304 => "Not Modified",
                400 => "Bad Request",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                500 => "Internal Server Error",
                _ => "",
            }
            .to_string()
        });
        let headers = init.headers.unwrap_or_default();
        let body_bytes = match body {
            Some(Either::A(s)) => s.into_bytes(),
            Some(Either::B(b)) => b.to_vec(),
            None => Vec::new(),
        };

        Response {
            status,
            status_text,
            headers,
            body: body_bytes,
            url: String::new(),
            redirected: false,
        }
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

    #[napi(getter)]
    pub fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        String::from_utf8(self.body.clone())
            .map_err(|e| napi::Error::new(Status::GenericFailure, format!("invalid utf-8: {e}")))
    }

    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let s = self.text().await?;
        serde_json::from_str(&s).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to parse json: {e}"))
        })
    }

    #[napi]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        Ok(self.body.clone().into())
    }

    #[napi]
    pub async fn bytes(&self) -> napi::Result<Buffer> {
        Ok(self.body.clone().into())
    }

    #[napi]
    pub fn clone(&self) -> Self {
        Response {
            status: self.status,
            status_text: self.status_text.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            url: self.url.clone(),
            redirected: self.redirected,
        }
    }

    /// Static constructor: `Response.json(data, init)`
    #[napi(factory, js_name = "json")]
    pub fn json_factory(data: serde_json::Value, init: Option<ResponseInit>) -> napi::Result<Self> {
        let json_str = serde_json::to_string(&data).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("json serialize: {e}"))
        })?;
        let mut resp_init = init.unwrap_or_default();
        let mut headers = resp_init.headers.unwrap_or_default();
        headers.entry("content-type".to_string()).or_insert_with(|| "application/json".to_string());
        resp_init.headers = Some(headers);
        Ok(Response::new(Some(Either::A(json_str)), Some(resp_init)))
    }

    /// Static constructor: `Response.redirect(url, status)`
    #[napi(factory)]
    pub fn redirect(url: String, status: Option<u16>) -> napi::Result<Self> {
        let status = status.unwrap_or(302);
        let mut headers = HashMap::new();
        headers.insert("location".to_string(), url);
        Ok(Response::new(
            None,
            Some(ResponseInit {
                status: Some(status),
                status_text: None,
                headers: Some(headers),
            }),
        ))
    }

    /// Static constructor: `Response.error()`
    #[napi(factory)]
    pub fn error() -> Self {
        Response {
            status: 0,
            status_text: String::new(),
            headers: HashMap::new(),
            body: Vec::new(),
            url: String::new(),
            redirected: false,
        }
    }
}

pub(crate) fn response_from_http_response(hr: http_request::HttpResponse, url: String) -> Response {
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
        body,
        url,
        redirected: hr.is_redirect(),
    }
}

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
    pub fn new(
        input: Either<String, &Request>,
        init: Option<RequestInit>,
    ) -> napi::Result<Self> {
        let mut init = init.unwrap_or_default();
        let (url, default_method, default_headers, default_body) = match input {
            Either::A(u) => (u, "GET".to_string(), HashMap::new(), None),
            Either::B(r) => (
                r.url.clone(),
                r.method.clone(),
                r.headers.clone(),
                r.body.clone(),
            ),
        };

        let method = init
            .method
            .take()
            .unwrap_or(default_method)
            .to_uppercase();

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

        Ok(Request {
            url,
            method,
            headers,
            body,
            mode: init.mode.unwrap_or_else(|| "cors".to_string()),
            credentials: init.credentials.unwrap_or_else(|| "same-origin".to_string()),
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
    pub fn clone(&self) -> Self {
        Request {
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
            body_used: Arc::new(AtomicBool::new(self.body_used())),
        }
    }

    #[napi]
    pub async fn text(&self) -> napi::Result<String> {
        self.body_used.store(true, Ordering::SeqCst);
        match &self.body {
            Some(b) => String::from_utf8(b.clone()).map_err(|e| {
                napi::Error::new(Status::GenericFailure, format!("invalid utf-8: {e}"))
            }),
            None => Ok(String::new()),
        }
    }

    #[napi(js_name = "json")]
    pub async fn parse_json(&self) -> napi::Result<serde_json::Value> {
        let t = self.text().await?;
        serde_json::from_str(&t).map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("failed to parse json: {e}"))
        })
    }

    #[napi]
    pub async fn array_buffer(&self) -> napi::Result<Buffer> {
        self.body_used.store(true, Ordering::SeqCst);
        Ok(self.body.clone().unwrap_or_default().into())
    }

    /// Instance method `start` using `http_request` to execute this Request.
    #[napi]
    pub async fn start(&self) -> napi::Result<Response> {
        execute_http_request_with_redirect(
            &self.method,
            &self.url,
            &self.headers,
            self.body.as_deref(),
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
    use http_request::{HttpRequest, Method, Body};

    let mut req = match method.to_uppercase().as_str() {
        "GET" => HttpRequest::get(url),
        "POST" => HttpRequest::post(url),
        m => {
            let mut r = HttpRequest::get(url);
            let parsed_method = match m {
                "PUT" => Method::Put,
                "DELETE" => Method::Delete,
                "PATCH" => Method::Patch,
                "HEAD" => Method::Head,
                "OPTIONS" => Method::Options,
                "CONNECT" => Method::Connect,
                "TRACE" => Method::Trace,
                other => Method::Unknown(other.to_string()),
            };
            r.set_method(parsed_method);
            r
        }
    };

    for (k, v) in headers {
        req.set_header(k.as_str(), v.as_str());
    }

    if let Some(b) = body {
        req.set_body(Body::from_bytes(b.to_vec()));
    }
    req.get_mut_config().set_timeout(30_000);

    let url_string = url.to_string();

    let mut current_url = url_string;
    let mut current_method = method.to_string();
    let mut redirect_count = 0;
    let max_redirects = 10;

    loop {
        let mut cur_req = req.clone();
        cur_req.set_url(&current_url);
        let cur_url_str = current_url.clone();

        let outcome = tokio::task::spawn_blocking(move || {
            cur_req.send()
        })
        .await
        .map_err(|e| napi::Error::new(Status::GenericFailure, format!("tokio spawn failed: {e}")))?
        .map_err(|e| napi::Error::new(Status::GenericFailure, format!("http request failed: {e:?}")))?;

        let resp = response_from_http_response(outcome, cur_url_str);
        if resp.status >= 300 && resp.status < 400 {
            if redirect_policy == "error" {
                return Err(napi::Error::new(Status::GenericFailure, "redirect error"));
            } else if redirect_policy == "manual" {
                return Ok(resp);
            } else if redirect_policy == "follow" {
                if let Some(loc) = resp.headers.get("location") {
                    redirect_count += 1;
                    if redirect_count > max_redirects {
                        return Err(napi::Error::new(Status::GenericFailure, "maximum redirect limit exceeded"));
                    }
                    current_url = loc.clone();
                    if resp.status == 303 {
                        current_method = "GET".to_string();
                    }
                    continue;
                }
            }
        }
        return Ok(resp);
    }
}

pub type RequestOpts = RequestInit;
