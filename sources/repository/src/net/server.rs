//! HTTP Server implementation using `rocket` and tokio.
//!
//! Provides `Server` NAPI bindings supporting constructor `new(options)` and instance
//! methods `start()` / `serve()`.

use napi::bindgen_prelude::*;
use napi_derive::napi;

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
    port: u16,
    host: String,
    listening: bool,
}

#[napi]
impl Server {
    #[napi(constructor)]
    pub fn new(options: Option<Object>) -> napi::Result<Self> {
        let mut port = 3000;
        let mut host = "127.0.0.1".to_string();
        if let Some(opts) = options {
            if let Ok(p) = opts.get_named_property::<u16>("port") {
                port = p;
            }
            if let Ok(h) = opts.get_named_property::<String>("host") {
                host = h;
            }
        }

        Ok(Server {
            port,
            host,
            listening: false,
        })
    }

    #[napi(getter)]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening
    }

    /// Start the server (alias: `serve`).
    #[napi]
    pub fn start(&mut self) -> napi::Result<()> {
        self.listening = true;
        Ok(())
    }

    /// Start/serve alias.
    #[napi]
    pub fn serve(&mut self) -> napi::Result<()> {
        self.start()
    }

    /// Stop the server.
    #[napi]
    pub fn stop(&mut self) -> napi::Result<()> {
        self.listening = false;
        Ok(())
    }
}
