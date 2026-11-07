//! NAPI bindings providing a Node.js `node:net` compatible API.
//!
//! Exposes `Socket`, `NetServer`, `createNetServer`, `connectSocket`.
//! Native TCP primitives. The public Node-compatible stream/event API is in main.ts.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// A TCP socket wrapping independent read and write halves.
#[napi]
pub struct Socket {
    read_half: Arc<Mutex<Option<OwnedReadHalf>>>,
    write_half: Arc<Mutex<Option<OwnedWriteHalf>>>,
    local_address: Arc<std::sync::RwLock<Option<String>>>,
    local_port: Arc<AtomicU16>,
    remote_address: Arc<std::sync::RwLock<Option<String>>>,
    remote_port: Arc<AtomicU16>,
    destroyed: Arc<AtomicBool>,
    cancelled: tokio::sync::watch::Sender<bool>,
}

#[napi]
impl Socket {
    #[napi(constructor)]
    pub fn new() -> Self {
        Socket {
            read_half: Arc::new(Mutex::new(None)),
            write_half: Arc::new(Mutex::new(None)),
            local_address: Arc::new(std::sync::RwLock::new(None)),
            local_port: Arc::new(AtomicU16::new(0)),
            remote_address: Arc::new(std::sync::RwLock::new(None)),
            remote_port: Arc::new(AtomicU16::new(0)),
            destroyed: Arc::new(AtomicBool::new(false)),
            cancelled: tokio::sync::watch::channel(false).0,
        }
    }

    pub(crate) fn from_tcp_stream(stream: TcpStream) -> Self {
        let local = stream.local_addr().ok();
        let peer = stream.peer_addr().ok();
        let (rh, wh) = stream.into_split();
        Socket {
            read_half: Arc::new(Mutex::new(Some(rh))),
            write_half: Arc::new(Mutex::new(Some(wh))),
            local_address: Arc::new(std::sync::RwLock::new(local.map(|a| a.ip().to_string()))),
            local_port: Arc::new(AtomicU16::new(local.map(|a| a.port()).unwrap_or(0))),
            remote_address: Arc::new(std::sync::RwLock::new(peer.map(|a| a.ip().to_string()))),
            remote_port: Arc::new(AtomicU16::new(peer.map(|a| a.port()).unwrap_or(0))),
            destroyed: Arc::new(AtomicBool::new(false)),
            cancelled: tokio::sync::watch::channel(false).0,
        }
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    #[napi(getter)]
    pub fn remote_address(&self) -> Option<String> {
        self.remote_address.read().ok()?.clone()
    }

    #[napi(getter)]
    pub fn remote_port(&self) -> Option<u16> {
        let p = self.remote_port.load(Ordering::SeqCst);
        if p == 0 { None } else { Some(p) }
    }

    #[napi(getter)]
    pub fn local_address(&self) -> Option<String> {
        self.local_address.read().ok()?.clone()
    }

    #[napi(getter)]
    pub fn local_port(&self) -> Option<u16> {
        let p = self.local_port.load(Ordering::SeqCst);
        if p == 0 { None } else { Some(p) }
    }

    /// Connect to a remote TCP server.
    #[napi]
    pub async fn connect(&self, port: u16, host: Option<String>) -> napi::Result<()> {
        let host = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).await.map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("connect failed: {e}"))
        })?;

        if let Ok(local) = stream.local_addr() {
            *self.local_address.write().unwrap() = Some(local.ip().to_string());
            self.local_port.store(local.port(), Ordering::SeqCst);
        }
        if let Ok(peer) = stream.peer_addr() {
            *self.remote_address.write().unwrap() = Some(peer.ip().to_string());
            self.remote_port.store(peer.port(), Ordering::SeqCst);
        }

        let (rh, wh) = stream.into_split();
        let mut r_lock = self.read_half.lock().await;
        *r_lock = Some(rh);
        let mut w_lock = self.write_half.lock().await;
        *w_lock = Some(wh);
        self.cancelled.send_replace(false);
        self.destroyed.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Write data to the socket without blocking read().
    #[napi]
    pub async fn write(&self, data: Either<String, Buffer>) -> napi::Result<u32> {
        if self.destroyed.load(Ordering::SeqCst) {
            return Err(napi::Error::new(
                napi::Status::GenericFailure,
                "socket is destroyed",
            ));
        }
        let bytes = match data {
            Either::A(s) => s.into_bytes(),
            Either::B(b) => b.to_vec(),
        };
        let mut lock = self.write_half.lock().await;
        let stream = lock.as_mut().ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "socket not connected")
        })?;
        let mut cancelled = self.cancelled.subscribe();
        if *cancelled.borrow() {
            return Err(napi::Error::from_reason("socket is destroyed"));
        }
        tokio::select! {
            _ = cancelled.changed() => return Err(napi::Error::from_reason("socket is destroyed")),
            result = stream.write_all(&bytes) => result,
        }
        .map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("write failed: {e}"))
        })?;
        Ok(bytes.len() as u32)
    }

    /// Read available data from the socket without blocking write().
    #[napi]
    pub async fn read(&self, size: Option<u32>) -> napi::Result<Option<Buffer>> {
        if self.destroyed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let sz = size.unwrap_or(65536) as usize;
        let mut buf = vec![0u8; sz];
        let mut lock = self.read_half.lock().await;
        if self.destroyed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let stream = lock.as_mut().ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "socket not connected")
        })?;
        let mut cancelled = self.cancelled.subscribe();
        if *cancelled.borrow() {
            return Ok(None);
        }
        let n = tokio::select! {
            _ = cancelled.changed() => return Ok(None),
            result = stream.read(&mut buf) => result,
        }
        .map_err(|e| napi::Error::new(napi::Status::GenericFailure, format!("read failed: {e}")))?;
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
        let mut lock = self.write_half.lock().await;
        if let Some(mut stream) = lock.take() {
            let _ = stream.shutdown().await;
        }
        Ok(())
    }

    /// Destroy the socket immediately and close the TCP connection.
    #[napi]
    pub async fn destroy(&self) -> napi::Result<()> {
        self.destroyed.store(true, Ordering::SeqCst);
        self.cancelled.send_replace(true);
        *self.read_half.lock().await = None;
        if let Some(mut stream) = self.write_half.lock().await.take() {
            let _ = stream.shutdown().await;
        }
        Ok(())
    }
}

/// A TCP server wrapping `tokio::net::TcpListener`.
#[napi]
pub struct NetServer {
    accepting: tokio::sync::RwLock<()>,
    listener: Arc<Mutex<Option<Arc<TcpListener>>>>,
    listening: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
    cancelled: tokio::sync::watch::Sender<bool>,
}

#[napi]
impl NetServer {
    #[napi(constructor)]
    pub fn new() -> Self {
        NetServer {
            accepting: tokio::sync::RwLock::new(()),
            listener: Arc::new(Mutex::new(None)),
            listening: Arc::new(AtomicBool::new(false)),
            port: Arc::new(AtomicU16::new(0)),
            cancelled: tokio::sync::watch::channel(false).0,
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
        if l.is_some() {
            return Err(napi::Error::from_reason("server already listening"));
        }
        *l = Some(Arc::new(listener));
        self.cancelled.send_replace(false);
        self.listening.store(true, Ordering::SeqCst);

        Ok(bound_port)
    }

    /// Accept the next incoming connection.
    #[napi]
    pub async fn accept(&self) -> napi::Result<Socket> {
        let _accepting = self.accepting.read().await;
        if !self.listening.load(Ordering::SeqCst) {
            return Err(napi::Error::new(
                napi::Status::GenericFailure,
                "server not listening",
            ));
        }
        let listener = self.listener.lock().await.clone().ok_or_else(|| {
            napi::Error::new(napi::Status::GenericFailure, "server not listening")
        })?;

        let mut cancelled = self.cancelled.subscribe();
        if *cancelled.borrow() {
            return Err(napi::Error::from_reason("server closed"));
        }
        let (stream, _peer) = tokio::select! {
            _ = cancelled.changed() => return Err(napi::Error::from_reason("server closed")),
            result = listener.accept() => result,
        }
        .map_err(|e| {
            napi::Error::new(napi::Status::GenericFailure, format!("accept failed: {e}"))
        })?;

        Ok(Socket::from_tcp_stream(stream))
    }

    /// Close the server and drop the listener socket immediately so port is released.
    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.listening.store(false, Ordering::SeqCst);
        self.port.store(0, Ordering::SeqCst);
        self.cancelled.send_replace(true);
        let _accepting = self.accepting.write().await;
        *self.listener.lock().await = None;
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
