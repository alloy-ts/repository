//! Local Axum transport for an asynchronous, runtime-independent Rust handler.
use alloy_server_core::{Request, Response, prepare_response};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderName, HeaderValue, Response as HttpResponse, StatusCode},
    routing::any,
};
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Options {
    pub address: IpAddr,
    pub port: u16,
    pub max_request_bytes: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            address: Ipv4Addr::LOCALHOST.into(),
            port: 8080,
            max_request_bytes: 16 * 1024 * 1024,
        }
    }
}
type Application = dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync;
#[derive(Clone)]
struct Dispatch {
    app: Arc<Application>,
    max_request_bytes: usize,
}

/// Build a router without starting a listener. The caller owns sockets and shutdown.
pub fn router<H, F>(max_request_bytes: usize, handler: H) -> Router
where
    H: Fn(Request) -> F + Send + Sync + 'static,
    F: Future<Output = Response> + Send + 'static,
{
    let app: Arc<Application> = Arc::new(move |request| Box::pin(handler(request)));
    Router::new().fallback(any(dispatch)).with_state(Dispatch {
        app,
        max_request_bytes,
    })
}
pub async fn serve<H, F>(options: Options, handler: H) -> std::io::Result<()>
where
    H: Fn(Request) -> F + Send + Sync + 'static,
    F: Future<Output = Response> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind((options.address, options.port)).await?;
    axum::serve(listener, router(options.max_request_bytes, handler)).await
}
fn error(status: StatusCode) -> HttpResponse<Body> {
    HttpResponse::builder()
        .status(status)
        .body(Body::empty())
        .unwrap()
}
async fn dispatch(
    State(state): State<Dispatch>,
    request: axum::http::Request<Body>,
) -> HttpResponse<Body> {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, state.max_request_bytes).await {
        Ok(body) => body.to_vec(),
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let response = prepare_response(
        (state.app)(Request::from_parts(parts.clone(), body)).await,
        parts.method == axum::http::Method::HEAD,
    );
    if response.status().is_informational() {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut output = HttpResponse::builder().status(response.status());
    for (name, value) in response.headers() {
        if alloy_server_core::is_transport_header(name.as_str()) {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::try_from(value.as_bytes()),
        ) else {
            return error(StatusCode::INTERNAL_SERVER_ERROR);
        };
        output = output.header(name, value);
    }
    output
        .body(Body::from(response.into_body()))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_server_core::http;
    use axum::http::{Method, Request as HttpRequest};
    use tower::ServiceExt;
    #[tokio::test]
    async fn generic_handler_preserves_binary_and_duplicate_headers() {
        let app = router(8, |request| async move {
            assert_eq!(request.uri().to_string(), "/echo?q=1&q=2");
            assert_eq!(request.headers()["x-input"], "value");
            http::Response::builder()
                .status(201)
                .header("set-cookie", "a=1")
                .header("set-cookie", "b=2")
                .body(request.into_body())
                .unwrap()
        });
        let request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/echo?q=1&q=2")
            .header("x-input", "value")
            .body(Body::from(vec![0, 255, 1]))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers().get_all("set-cookie").iter().count(), 2);
        assert_eq!(
            to_bytes(response.into_body(), 8).await.unwrap().as_ref(),
            &[0, 255, 1]
        );
    }
    #[tokio::test]
    async fn rejects_oversized_request_before_dispatch() {
        let app = router(2, |_request| async { panic!("must not dispatch") });
        let request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/")
            .body(Body::from(vec![0, 1, 2]))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
