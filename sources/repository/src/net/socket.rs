//! NAPI bindings providing a Node.js `node:net` compatible API.
//!
//! Exposes `Socket`, `NetServer`, `createNetServer`, `connectSocket`.
//! Conforms to https://nodejs.org/api/net.html

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use tokio::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use napi::bindgen_prelude::*;
use napi_derive::napi;

/// A TCP socket wrapping `tokio::net::TcpStream`.
#[napi]
pub struct Socket {
    stream: Arc<Mutex<Option<TcpStream>>>,
    local_address: Option<String>,
    local_port: Arc<AtomicU16>,
    remote_address: Option<String>,
    remote_port: Arc<AtomicU16>,
    destroyed: Arc<AtomicBool>,
}

#[napi]
impl Socket {
    #[napi(constructor)]
    pub fn new() -> Self {
        Socket {
            stream: Arc::new(Mutex::new(None)),
            local_address: None,
            local_port: Arc::new(AtomicU16::new(0)),
            remote_address: None,
            remote_port: Arc::new(AtomicU16::new(0)),
            destroyed: Arc::new(AtomicBool::new(false)),
        }
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn remote_address(&self) -> Option<String> {
        self.remote_address.clone()
    }

    #[napi(getter)]
    pub fn remote_port(&self) -> Option<u16> {
        let p = self.remote_port.load(Ordering::SeqCst);
        if p == 0 { None } else { Some(p) }
    }

    #[napi(getter)]
    pub fn local_address(&self) -> Option<String> {
        self.local_address.clone()
    }

    #[napi(getter)]
    pub fn local_port(&self) -> Option<u16> {
        let p = self.local_port.load(Ordering::SeqCst);
        if p == 0 { None } else { Some(p) }
    }

    /// Connect to a remote TCP server.
    #[napi]
    pub async fn connect(
        &self,
        port: u16,
        host: Option<String>,
    ) -> napi::Result<()> {
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("connect failed: {e}"))
        })?;

        if let Ok(local) = stream.local_addr() {
            self.local_port.store(local.port(), Ordering::SeqCst);
        }
        if let Ok(peer) = stream.peer_addr() {
            self.remote_port.store(peer.port(), Ordering::SeqCst);
        }

        let mut lock = self.stream.lock().await;
        *lock = Some(stream);
        self.destroyed.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Write data to the socket.
    #[napi]
    pub async fn write(&self, data: Either<String, Buffer>) -> napi::Result<u32> {
        let bytes = match data {
            Either::A(s) => s.into_bytes(),
            Either::B(b) => b.to_vec(),
        };
        let mut lock = self.stream.lock().await;
        let stream = lock.as_mut().ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "socket not connected")
        })?;
        stream.write_all(&bytes).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("write failed: {e}"))
        })?;
        Ok(bytes.len() as u32)
    }

    /// Read available data from the socket.
    #[napi]
    pub async fn read(&self, size: Option<u32>) -> napi::Result<Option<Buffer>> {
        let sz = size.unwrap_or(65536) as usize;
        let mut buf = vec![0u8; sz];
        let mut lock = self.stream.lock().await;
        let stream = lock.as_mut().ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "socket not connected")
        })?;
        let n = stream.read(&mut buf).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("read failed: {e}"))
        })?;
        if n == 0 {
            Ok(None)
        } else {
            buf.truncate(n);
            Ok(Some(buf.into()))
        }
    }

    /// Half-close the socket (or fully close if both ends end).
    #[napi]
    pub async fn end(&self, data: Option<Either<String, Buffer>>) -> napi::Result<()> {
        if let Some(d) = data {
            self.write(d).await?;
        }
        let mut lock = self.stream.lock().await;
        if let Some(stream) = lock.as_mut() {
            let _ = stream.shutdown().await;
        }
        self.destroyed.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Destroy the socket immediately.
    #[napi]
    pub fn destroy(&self) -> napi::Result<()> {
        self.destroyed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// A TCP server wrapping `tokio::net::TcpListener`.
#[napi]
pub struct NetServer {
    listener: Arc<Mutex<Option<Arc<TcpListener>>>>,
    listening: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
}

#[napi]
impl NetServer {
    #[napi(constructor)]
    pub fn new() -> Self {
        NetServer {
            listener: Arc::new(Mutex::new(None)),
            listening: Arc::new(AtomicBool::new(false)),
            port: Arc::new(AtomicU16::new(0)),
        }
    }

    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn port(&self) -> Option<u16> {
        let p = self.port.load(Ordering::SeqCst);
        if p == 0 { None } else { Some(p) }
    }

    /// Begin listening for connections.
    #[napi]
    pub async fn listen(&self, port: u16, host: Option<String>) -> napi::Result<u16> {
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let addr = format!("{}:{}", host, port);
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("listen failed: {e}"))
        })?;

        let bound_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
        self.port.store(bound_port, Ordering::SeqCst);
        let mut l = self.listener.lock().await;
        *l = Some(Arc::new(listener));
        self.listening.store(true, Ordering::SeqCst);

        Ok(bound_port)
    }

    /// Accept the next incoming connection.
    #[napi]
    pub async fn accept(&self) -> napi::Result<Socket> {
        let listener_opt = self.listener.lock().await.clone();
        let listener = listener_opt.ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "server not listening")
        })?;

        let (stream, peer) = listener.accept().await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("accept failed: {e}"))
        })?;

        let local = stream.local_addr().ok();

        Ok(Socket {
            stream: Arc::new(Mutex::new(Some(stream))),
            local_address: local.map(|a| a.ip().to_string()),
            local_port: Arc::new(AtomicU16::new(local.map(|a| a.port()).unwrap_or(0))),
            remote_address: Some(peer.ip().to_string()),
            remote_port: Arc::new(AtomicU16::new(peer.port())),
            destroyed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Close the server.
    #[napi]
    pub fn close(&self) -> napi::Result<()> {
        self.listening.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// Helper function to create a new `NetServer`.
#[napi]
pub fn create_net_server() -> NetServer {
    NetServer::new()
}

/// Helper function to connect to a TCP server.
#[napi]
pub async fn connect_socket(port: u16, host: Option<String>) -> napi::Result<Socket> {
    let sock = Socket::new();
    sock.connect(port, host).await?;
    Ok(sock)
}
