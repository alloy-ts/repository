use lambda_http::{Body, Error, Request, Response, run, service_fn};

async fn handle(request: Request) -> Result<Response<Body>, Error> {
    let (parts, body) = request.into_parts();
    let bytes = match body {
        Body::Empty => Vec::new(),
        Body::Text(text) => text.into_bytes(),
        Body::Binary(bytes) => bytes,
        _ => return Err("unsupported Lambda body encoding".into()),
    };
    if bytes.len() > 4 * 1024 * 1024 {
        return Ok(Response::builder().status(413).body(Body::Empty)?);
    }
    let response =
        cloud_example_app::handle(alloy_server_core::Request::from_parts(parts, bytes)).await;
    let (parts, bytes) = response.into_parts();
    Ok(Response::from_parts(parts, Body::Binary(bytes)))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_application_preserves_binary_and_head_semantics() {
        let request = lambda_http::http::Request::builder()
            .method("POST")
            .uri("https://example.test/echo")
            .body(Body::Binary(vec![0, 255]))
            .unwrap();
        let response = handle(request).await.unwrap();
        assert_eq!(response.body(), &Body::Binary(vec![0, 255]));
        let request = lambda_http::http::Request::builder()
            .method("HEAD")
            .uri("https://example.test/health")
            .body(Body::Empty)
            .unwrap();
        let response = handle(request).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), &Body::Binary(vec![]));
    }
}
