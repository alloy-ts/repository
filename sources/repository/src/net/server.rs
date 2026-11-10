//! Rocket transport for the public Server routing adapter (net/server.ts).
//!
//! Rocket 0.5.2's `Method` is a closed enum: Get, Put, Post, Delete, Options,
//! Head, Trace, Connect, and Patch. `FromStr` rejects anything else, and
//! `Request::from_hyp` turns that into HTTP 400 before a route runs. The
//! unpublished 0.6 tree adds IANA registry methods, but not QUERY, PURGE, or
//! arbitrary tokens, so upgrading would not serve those verbs. `server.query`
//! and `server.on` still dispatch them through `fetch()` / `request()`.
//! A listener answers QUERY, PURGE, and other unknown methods with Rocket's 400.
use base64::{Engine, engine::general_purpose::STANDARD};
use napi::{bindgen_prelude::*, threadsafe_function::ThreadsafeFunction};
use napi_derive::napi;
use rocket::{
    Data, Request, Route,
    data::{Limits, ToByteUnit},
    http::{Header, Method, Status},
    route::{Handler, Outcome},
};

/// Request body cap. Also applied to Rocket's named limits below.
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
};
use tokio::sync::{Mutex, oneshot};

type Callback = ThreadsafeFunction<String, Promise<String>, String, napi::Status, false, true>;
#[derive(Clone)]
struct Dispatch(Arc<Callback>);
#[rocket::async_trait]
impl Handler for Dispatch {
    async fn handle<'r>(&self, req: &'r Request<'_>, data: Data<'r>) -> Outcome<'r> {
        for value in req.headers().get("Content-Length") {
            match value.parse::<u64>() {
                Ok(len) if len > MAX_BODY_BYTES => return Outcome::Error(Status::PayloadTooLarge),
                Ok(_) => {}
                Err(_) => return Outcome::Error(Status::BadRequest),
            }
        }
        let bytes = match data.open((MAX_BODY_BYTES as usize).bytes()).into_bytes().await {
            Ok(bytes) if bytes.is_complete() => bytes.into_inner(),
            _ => return Outcome::Error(Status::PayloadTooLarge),
        };
        let headers: Vec<_> = req
            .headers()
            .iter()
            .map(|h| (h.name().to_string(), h.value().to_string()))
            .collect();
        let input = serde_json::json!({"port":req.rocket().config().port, "method":req.method().as_str(), "path":req.uri().to_string(), "headers":headers, "body":STANDARD.encode(bytes)}).to_string();
        let result = async {
            let promise = self.0.call_async_catch(input).await?;
            promise.await
        }
        .await;
        let output: serde_json::Value = match result.and_then(|s| {
            serde_json::from_str(&s).map_err(|e| napi::Error::from_reason(e.to_string()))
        }) {
            Ok(v) => v,
            Err(_) => return Outcome::Error(Status::InternalServerError),
        };
        let status = output["status"].as_u64().unwrap_or(500) as u16;
        let body = match STANDARD.decode(output["body"].as_str().unwrap_or("")) {
            Ok(b) => b,
            Err(_) => return Outcome::Error(Status::InternalServerError),
        };
        let mut response = rocket::Response::build();
        response.status(Status::new(status));
        let mut declared_length: Option<usize> = None;
        if let Some(headers) = output["headers"].as_array() {
            for pair in headers {
                if let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str()) {
                    // Rocket owns transfer framing. A HEAD response keeps the
                    // handler's content-length and sends no payload.
                    if k.eq_ignore_ascii_case("content-length") {
                        declared_length = v.parse().ok();
                        continue;
                    }
                    if matches!(k.to_ascii_lowercase().as_str(), "transfer-encoding" | "connection") {
                        continue;
                    }
                    response.header_adjoin(Header::new(k.to_owned(), v.to_owned()));
                }
            }
        }
        if req.method() == Method::Head {
            let length = declared_length.unwrap_or(body.len());
            response.sized_body(Some(length), Cursor::new(Vec::<u8>::new()));
        } else {
            response.sized_body(body.len(), Cursor::new(body));
        }
        Outcome::Success(response.finalize())
    }
}

struct Running {
    shutdown: rocket::Shutdown,
    task: tokio::task::JoinHandle<()>,
}
#[napi(js_name = "RocketServer")]
pub struct Server {
    port: Arc<AtomicU16>,
    host: String,
    listening: Arc<AtomicBool>,
    callback: Arc<Callback>,
    running: Mutex<Option<Running>>,
}
#[napi]
impl Server {
    #[napi(constructor)]
    pub fn new(
        port: u16,
        host: String,
        handler: Function<String, Promise<String>>,
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
        })
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
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Err(napi::Error::from_reason("server already started"));
        }
        let address = self
            .host
            .parse::<std::net::IpAddr>()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let mut config = rocket::Config::debug_default();
        config.address = address;
        config.port = self.port();
        config.log_level = rocket::config::LogLevel::Off;
        config.shutdown.ctrlc = false;
        config.shutdown.grace = 0;
        config.shutdown.mercy = 0;
        config.limits = Limits::default()
            .limit("bytes", (MAX_BODY_BYTES as usize).bytes())
            .limit("string", (MAX_BODY_BYTES as usize).bytes())
            .limit("data-form", (MAX_BODY_BYTES as usize).bytes())
            .limit("file", (MAX_BODY_BYTES as usize).bytes())
            .limit("json", (MAX_BODY_BYTES as usize).bytes())
            .limit("form", (MAX_BODY_BYTES as usize).bytes());
        let (ready_tx, ready_rx) = oneshot::channel();
        let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
        let port = self.port.clone();
        let listening = self.listening.clone();
        // Every variant Rocket 0.5 can parse. Unknown methods never reach a route.
        let routes = [
            Method::Get,
            Method::Head,
            Method::Post,
            Method::Put,
            Method::Delete,
            Method::Patch,
            Method::Options,
            Method::Trace,
            Method::Connect,
        ]
        .into_iter()
        .flat_map(|method| {
            let handler = Dispatch(self.callback.clone());
            [
                Route::new(method, "/", handler.clone()),
                Route::new(method, "/<path..>", handler),
            ]
        })
        .collect::<Vec<_>>();
        let rocket = rocket::custom(config)
            .mount("/", routes)
            .attach(rocket::fairing::AdHoc::on_liftoff("ready", move |rocket| {
                let tx = ready_tx.clone();
                let port = port.clone();
                let listening = listening.clone();
                Box::pin(async move {
                    port.store(rocket.config().port, Ordering::SeqCst);
                    listening.store(true, Ordering::SeqCst);
                    if let Some(tx) = tx.lock().await.take() {
                        let _ = tx.send(rocket.config().port);
                    }
                })
            }))
            .ignite()
            .await
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let shutdown = rocket.shutdown();
        let flag = self.listening.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = rocket.launch().await {
                eprintln!("Rocket server: {error}");
            }
            flag.store(false, Ordering::SeqCst);
        });
        match ready_rx.await {
            Ok(port) => {
                *running = Some(Running { shutdown, task });
                Ok(port)
            }
            Err(_) => {
                let _ = task.await;
                Err(napi::Error::from_reason("Rocket failed to bind"))
            }
        }
    }
    #[napi]
    pub async fn serve(&self) -> napi::Result<u16> {
        self.start().await
    }
    #[napi]
    pub async fn stop(&self) -> napi::Result<()> {
        let mut running = self.running.lock().await;
        if let Some(server) = running.take() {
            server.shutdown.notify();
            server
                .task
                .await
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        }
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(server) = self.running.get_mut().take() {
            server.shutdown.notify();
        }
    }
}
