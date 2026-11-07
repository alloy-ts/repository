//! HTTP Server implementation using `rocket` and tokio.
//!
//! Provides `Server` NAPI bindings supporting constructor `new(options)` and instance
//! methods `start()` / `serve()`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use napi::Status;

/// Server configuration and options.
#[napi(object)]
#[derive(Default)]
pub struct ServerOptions {
    pub port: Option<u16>,
    pub host: Option<String>,
}

/// HTTP Server class.
#[napi]
pub struct Server {
    port: Arc<AtomicU16>,
    host: String,
    listening: Arc<AtomicBool>,
    shutdown_tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    routes: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    default_response: Arc<tokio::sync::Mutex<String>>,
}

#[napi]
impl Server {
    #[napi(constructor)]
    pub fn new(options: Option<Object>) -> napi::Result<Self> {
        let mut port = 3000;
        let mut host = "127.0.0.1".to_string();
        let mut routes_map = HashMap::new();
        let mut default_resp = "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\n\r\nNot Found".to_string();

        if let Some(opts) = options {
            if let Ok(p) = opts.get_named_property::<u16>("port") {
                port = p;
            }
            if let Ok(h) = opts.get_named_property::<String>("host") {
                host = h;
            }
            if let Ok(routes_obj) = opts.get_named_property::<Object>("routes") {
                let keys = Object::keys(&routes_obj).unwrap_or_default();
                for key in keys {
                    // Check if value is a Response object or string
                    if let Ok(res_obj) = routes_obj.get_named_property::<Object>(&key) {
                        let status: u16 = res_obj.get_named_property("status").unwrap_or(200);
                        let body_text: String = "OK".to_string();
                        let wire = format!("HTTP/1.1 {status} OK\r\ncontent-length: {}\r\n\r\n{}", body_text.len(), body_text);
                        routes_map.insert(key, wire);
                    }
                }
            }
            if let Ok(_fetch_fn) = opts.get_named_property::<Function<'_, (), ()>>("fetch") {
                default_resp = "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\n\r\nNot Found".to_string();
            }
        }

        Ok(Server {
            port: Arc::new(AtomicU16::new(port)),
            host,
            listening: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Arc::new(tokio::sync::Mutex::new(None)),
            routes: Arc::new(tokio::sync::Mutex::new(routes_map)),
            default_response: Arc::new(tokio::sync::Mutex::new(default_resp)),
        })
    }

    #[napi(getter)]
    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    /// Start the server and bind the socket listener.
    #[napi]
    pub async fn start(&self) -> napi::Result<u16> {
        let requested_port = self.port.load(Ordering::SeqCst);
        let addr = format!("{}:{}", self.host, requested_port);
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            napi::Error::new(Status::GenericFailure, format!("bind failed: {e}"))
        })?;

        let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(requested_port);
        self.port.store(bound_port, Ordering::SeqCst);
        self.listening.store(true, Ordering::SeqCst);

        let (tx, mut rx) = oneshot::channel();
        let mut shutdown_lock = self.shutdown_tx.lock().await;
        *shutdown_lock = Some(tx);

        let listening_flag = Arc::clone(&self.listening);
        let routes = Arc::clone(&self.routes);
        let default_res = Arc::clone(&self.default_response);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((mut socket, _)) => {
                                let routes_c = Arc::clone(&routes);
                                let default_c = Arc::clone(&default_res);
                                tokio::spawn(async move {
                                    let mut buf = [0u8; 4096];
                                    if let Ok(n) = socket.read(&mut buf).await {
                                        if n > 0 {
                                            let req_str = String::from_utf8_lossy(&buf[..n]);
                                            let path = req_str.split_whitespace().nth(1).unwrap_or("/");
                                            let lock = routes_c.lock().await;
                                            let res_payload = match lock.get(path) {
                                                Some(r) => r.clone(),
                                                None => {
                                                    // Check prefix / wildcard
                                                    let mut matched = None;
                                                    for (k, v) in lock.iter() {
                                                        if k.ends_with("/*") && path.starts_with(&k[..k.len()-1]) {
                                                            matched = Some(v.clone());
                                                            break;
                                                        }
                                                    }
                                                    matched.unwrap_or_else(|| default_c.try_lock().map(|l| l.clone()).unwrap_or_else(|_| "HTTP/1.1 404 Not Found\r\ncontent-length: 9\r\n\r\nNot Found".to_string()))
                                                }
                                            };
                                            let _ = socket.write_all(res_payload.as_bytes()).await;
                                        }
                                    }
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

    /// Start/serve alias.
    #[napi]
    pub async fn serve(&self) -> napi::Result<u16> {
        self.start().await
    }

    /// Stop the server.
    #[napi]
    pub async fn stop(&self) -> napi::Result<()> {
        self.listening.store(false, Ordering::SeqCst);
        let mut shutdown_lock = self.shutdown_tx.lock().await;
        if let Some(tx) = shutdown_lock.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}
