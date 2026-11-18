use alloy_server_core::{Request, Response, prepare_response};

/// The same application function is compiled for local Axum, Lambda and Wasm Workers.
pub async fn handle(request: Request) -> Response {
    let head = request.method() == "HEAD";
    let response = match (request.method().as_str(), request.uri().path()) {
        ("GET" | "HEAD", "/health") => alloy_server_core::http::Response::builder()
            .header("content-type", "application/json")
            .body(br#"{"ok":true}"#.to_vec())
            .unwrap(),
        ("POST", "/echo") => Response::new(request.into_body()),
        _ => alloy_server_core::http::Response::builder()
            .status(404)
            .body(b"Not Found".to_vec())
            .unwrap(),
    };
    prepare_response(response, head)
}
