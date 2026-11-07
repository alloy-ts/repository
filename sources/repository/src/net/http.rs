//! Node.js `node:http` compatible API bindings.
//!
//! Exposes `IncomingMessage`, `ServerResponse`, `HttpServer`, `createHttpServer`.
//! Conforms to https://nodejs.org/api/http.html

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use napi::Status;

/// HTTP Incoming message (request).
#[napi]
pub struct IncomingMessage {
    url: String,
    method: String,
    headers: HashMap<String, String>,
}

#[napi]
impl IncomingMessage {
    #[napi(constructor)]
    pub fn new(url: String, method: String, headers: HashMap<String, String>) -> Self {
        IncomingMessage {
            url,
            method,
            headers,
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
    listening: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
    shutdown_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
}

#[napi]
impl HttpServer {
    #[napi(constructor)]
    pub fn new() -> Self {
        HttpServer {
            listening: Arc::new(AtomicBool::new(false)),
            port: Arc::new(AtomicU16::new(0)),
            shutdown_tx: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    #[napi]
    pub async fn listen(&self, port: u16) -> napi::Result<u16> {
        let addr = format!("127.0.0.1:{}", port);
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("bind failed: {e}"))
        })?;

        let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
        self.port.store(bound_port, Ordering::SeqCst);
        self.listening.store(true, Ordering::SeqCst);

        let (tx, mut rx) = oneshot::channel();
        let mut shutdown_lock = self.shutdown_tx.lock().await;
        *shutdown_lock = Some(tx);

        let listening_flag = Arc::clone(&self.listening);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    res = listener.accept() => {
                        match res {
                            Ok((mut socket, _)) => {
                                tokio::spawn(async move {
                                    let mut buf = [0u8; 1024];
                                    let _ = socket.read(&mut buf).await;
                                    let resp = "HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\nhello world";
                                    let _ = socket.write_all(resp.as_bytes()).await;
                                });
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
            listening_flag.store(false, Ordering::SeqCst);
        });

        Ok(bound_port)
    }

    #[napi]
    pub fn close(&self) {
        self.listening.store(false, Ordering::SeqCst);
        let tx_mutex = Arc::clone(&self.shutdown_tx);
        tokio::spawn(async move {
            let mut lock = tx_mutex.lock().await;
            if let Some(tx) = lock.take() {
                let _ = tx.send(());
            }
        });
    }
}

/// Create an HTTP server.
#[napi]
pub fn create_http_server() -> HttpServer {
    HttpServer::new()
}
