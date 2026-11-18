//! Axum transport for the public TypeScript Server adapter.
use super::routing;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderName, HeaderValue, Request, Response, StatusCode},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::stream;
use napi::{bindgen_prelude::*, threadsafe_function::ThreadsafeFunction};
use napi_derive::napi;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU16, Ordering},
};
use std::{collections::HashMap, sync::RwLock};
use tokio::sync::{Mutex, oneshot};

type Callback = ThreadsafeFunction<String, Promise<String>, String, napi::Status, false, true>;
#[derive(Clone)]
struct Dispatch {
    callback: Arc<Callback>,
    port: Arc<AtomicU16>,
}

struct ResponseStream {
    callback: Arc<Callback>,
    id: u64,
    done: bool,
}
impl Drop for ResponseStream {
    fn drop(&mut self) {
        let callback = self.callback.clone();
        let input = serde_json::json!({"op":"cancel", "id":self.id}).to_string();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Ok(promise) = callback.call_async_catch(input).await {
                    let _ = promise.await;
                }
            });
        }
    }
}
fn error(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}
async fn dispatch(State(state): State<Dispatch>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let headers: Vec<_> = parts
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or("")))
        .collect();
    let input = serde_json::json!({
        "port":state.port.load(Ordering::SeqCst), "method":parts.method.as_str(),
        "path":parts.uri.to_string(), "headers":headers, "body":STANDARD.encode(bytes)
    })
    .to_string();
    let result = async {
        let promise = state.callback.call_async_catch(input).await?;
        promise.await
    }
    .await;
    let output: serde_json::Value = match result.ok().and_then(|s| serde_json::from_str(&s).ok()) {
        Some(value) => value,
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let status = output["status"]
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .and_then(|n| StatusCode::from_u16(n).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = Response::builder().status(status);
    if let Some(headers) = output["headers"].as_array() {
        for pair in headers {
            let Some(pair) = pair.as_array().filter(|pair| pair.len() == 2) else {
                return error(StatusCode::INTERNAL_SERVER_ERROR);
            };
            let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str()) else {
                return error(StatusCode::INTERNAL_SERVER_ERROR);
            };
            if alloy_server_core::is_transport_header(k) {
                continue;
            }
            let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) else {
                return error(StatusCode::INTERNAL_SERVER_ERROR);
            };
            response = response.header(k, v);
        }
    }
    let body = if parts.method == axum::http::Method::HEAD
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
    {
        if let Some(id) = output["stream"].as_u64() {
            drop(ResponseStream {
                callback: state.callback,
                id,
                done: true,
            });
        }
        Body::empty()
    } else if let Some(id) = output["stream"].as_u64() {
        let body = stream::unfold(
            ResponseStream {
                callback: state.callback,
                id,
                done: false,
            },
            |mut state| async move {
                if state.done {
                    return None;
                }
                let input = serde_json::json!({"op":"read", "id":state.id}).to_string();
                let encoded = match state.callback.call_async_catch(input).await {
                    Ok(promise) => match promise.await {
                        Ok(value) => value,
                        Err(_) => {
                            return Some((
                                Err::<Vec<u8>, std::io::Error>(std::io::Error::other(
                                    "response stream failed",
                                )),
                                state,
                            ));
                        }
                    },
                    Err(_) => {
                        return Some((Err(std::io::Error::other("response stream failed")), state));
                    }
                };
                match STANDARD.decode(encoded) {
                    Ok(chunk) if chunk.is_empty() => None,
                    Ok(chunk) => Some((Ok(chunk), state)),
                    Err(_) => {
                        state.done = true;
                        Some((Err(std::io::Error::other("invalid response chunk")), state))
                    }
                }
            },
        );
        Body::from_stream(body)
    } else {
        match STANDARD.decode(output["body"].as_str().unwrap_or("")) {
            Ok(bytes) => Body::from(bytes),
            Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
        }
    };
    response
        .body(body)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

struct Running {
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
async fn reap_finished(running: &mut Option<Running>) {
    if running
        .as_ref()
        .is_some_and(|server| server.task.is_finished())
    {
        if let Some(server) = running.take() {
            let _ = server.task.await;
        }
    }
}
#[napi(js_name = "NativeServer")]
pub struct Server {
    port: Arc<AtomicU16>,
    host: String,
    listening: Arc<AtomicBool>,
    callback: Arc<Callback>,
    running: Mutex<Option<Running>>,
    routes: RwLock<Vec<routing::Route>>,
    strict: bool,
}
#[napi(object)]
pub struct MatchedRoute {
    pub handler_id: u32,
    pub params: HashMap<String, String>,
    pub is_middleware: bool,
    pub error_boundary_id: Option<u32>,
}
#[napi(object)]
pub struct RouteMatchResult {
    pub matches: Vec<MatchedRoute>,
}
#[napi]
impl Server {
    #[napi(constructor)]
    pub fn new(
        port: u16,
        host: String,
        handler: Function<String, Promise<String>>,
        strict: Option<bool>,
    ) -> napi::Result<Self> {
        Ok(Self {
            port: Arc::new(AtomicU16::new(port)),
            host,
            listening: Arc::new(AtomicBool::new(false)),
            callback: Arc::new(
                handler
                    .build_threadsafe_function::<String>()
                    .weak::<true>()
                    .build()?,
            ),
            running: Mutex::new(None),
            routes: RwLock::new(Vec::new()),
            strict: strict.unwrap_or(true),
        })
    }
    #[napi]
    pub fn add_route(
        &self,
        method: String,
        pattern: String,
        handler_id: u32,
        is_middleware: bool,
        error_boundary_id: Option<u32>,
    ) -> napi::Result<()> {
        let route = routing::Route::new(
            method,
            &pattern,
            handler_id,
            is_middleware,
            error_boundary_id,
            self.strict,
        )
        .map_err(napi::Error::from_reason)?;
        self.routes
            .write()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?
            .push(route);
        Ok(())
    }
    #[napi]
    pub fn match_route(&self, method: String, path: String) -> napi::Result<RouteMatchResult> {
        let routes = self
            .routes
            .read()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let matches = routes
            .iter()
            .filter_map(|route| route.matches(&method, &path))
            .map(|m| MatchedRoute {
                handler_id: m.handler_id,
                params: m.params,
                is_middleware: m.is_middleware,
                error_boundary_id: m.error_boundary_id,
            })
            .collect();
        Ok(RouteMatchResult { matches })
    }
    #[napi(getter)]
    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }
    #[napi(getter)]
    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }
    #[napi]
    pub async fn start(&self) -> napi::Result<u16> {
        self.start_with_router(Router::new()).await
    }
    #[napi]
    pub async fn serve(&self) -> napi::Result<u16> {
        self.start().await
    }
    #[napi]
    pub async fn stop(&self) -> napi::Result<()> {
        let mut running = self.running.lock().await;
        if let Some(mut server) = running.take() {
            let _ = server.shutdown.send(());
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut server.task)
                .await
                .is_err()
            {
                server.task.abort();
                let _ = server.task.await;
            }
            self.listening.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Server {
    /// Axum router for the local TypeScript request handler. Additional routers
    /// can be merged into it before binding a listener.
    pub fn into_router(&self) -> Router {
        Router::new().fallback(any(dispatch)).with_state(Dispatch {
            callback: self.callback.clone(),
            port: self.port.clone(),
        })
    }

    /// Start this server with extra Axum routes, such as an MCP transport.
    pub async fn start_with_router(&self, extra: Router) -> napi::Result<u16> {
        let mut running = self.running.lock().await;
        reap_finished(&mut running).await;
        if running.is_some() {
            return Err(napi::Error::from_reason("server already started"));
        }
        let address = self
            .host
            .parse::<std::net::IpAddr>()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let listener = tokio::net::TcpListener::bind((address, self.port()))
            .await
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?
            .port();
        self.port.store(port, Ordering::SeqCst);
        let app = self.into_router().merge(extra);
        let (shutdown, done) = oneshot::channel();
        let listening = self.listening.clone();
        self.listening.store(true, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = done.await;
                })
                .await
            {
                eprintln!("Axum server: {error}");
            }
            listening.store(false, Ordering::SeqCst);
        });
        *running = Some(Running { shutdown, task });
        Ok(port)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(server) = self.running.get_mut().take() {
            let _ = server.shutdown.send(());
        }
    }
}
