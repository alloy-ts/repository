//! Node.js `node:http` compatible API bindings.
//!
//! Exposes `IncomingMessage`, `ServerResponse`, `HttpServer`, `createHttpServer`.
//! Conforms to https://nodejs.org/api/http.html

use std::collections::HashMap;
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// HTTP Incoming message (request).
#[napi]
pub struct IncomingMessage {
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[napi]
impl IncomingMessage {
    #[napi(constructor)]
    pub fn new(url: String, method: String, headers: HashMap<String, String>) -> Self {
        IncomingMessage {
            url,
            method,
            headers,
            body: Vec::new(),
        }
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
}

/// HTTP Server response.
#[napi]
pub struct ServerResponse {
    status_code: u16,
    status_message: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    finished: bool,
}

#[napi]
impl ServerResponse {
    #[napi(constructor)]
    pub fn new() -> Self {
        ServerResponse {
            status_code: 200,
            status_message: "OK".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            finished: false,
        }
    }

    #[napi(getter)]
    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    #[napi(setter)]
    pub fn set_status_code(&mut self, code: u16) {
        self.status_code = code;
    }

    #[napi]
    pub fn set_header(&mut self, name: String, value: String) {
        self.headers.insert(name.to_lowercase(), value);
    }

    #[napi]
    pub fn get_header(&self, name: String) -> Option<String> {
        self.headers.get(&name.to_lowercase()).cloned()
    }

    #[napi]
    pub fn write_head(&mut self, status_code: u16, headers: Option<HashMap<String, String>>) {
        self.status_code = status_code;
        if let Some(h) = headers {
            for (k, v) in h {
                self.headers.insert(k.to_lowercase(), v);
            }
        }
    }

    #[napi]
    pub fn end(&mut self, data: Option<Either<String, Buffer>>) {
        if let Some(d) = data {
            let bytes = match d {
                Either::A(s) => s.into_bytes(),
                Either::B(b) => b.to_vec(),
            };
            self.body.extend_from_slice(&bytes);
        }
        self.finished = true;
    }
}

/// HTTP Server.
#[napi]
pub struct HttpServer {
    listening: bool,
    port: Option<u16>,
}

#[napi]
impl HttpServer {
    #[napi(constructor)]
    pub fn new() -> Self {
        HttpServer {
            listening: false,
            port: None,
        }
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening
    }

    #[napi]
    pub fn listen(&mut self, port: u16) -> u16 {
        self.listening = true;
        self.port = Some(port);
        port
    }

    #[napi]
    pub fn close(&mut self) {
        self.listening = false;
    }
}

/// Create an HTTP server.
#[napi]
pub fn create_http_server() -> HttpServer {
    HttpServer::new()
}
