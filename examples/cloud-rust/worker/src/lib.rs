use futures_util::StreamExt;
use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
pub async fn fetch(mut request: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let mut input = alloy_server_core::http::Request::builder()
        .method(request.method().to_string().as_str())
        .uri(request.url()?.as_str());
    for (name, value) in request.headers().entries() {
        input = input.header(name, value);
    }
    let mut body = Vec::new();
    if request.inner().body().is_some() {
        let mut stream = request.stream()?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > 4 * 1024 * 1024 {
                return Response::error("Payload Too Large", 413);
            }
            body.extend_from_slice(&chunk);
        }
    }
    let input = input
        .body(body)
        .map_err(|error| worker::Error::RustError(error.to_string()))?;
    let response = cloud_example_app::handle(input).await;
    let headers = worker::Headers::new();
    for (name, value) in response.headers() {
        headers.append(
            name.as_str(),
            value
                .to_str()
                .map_err(|error| worker::Error::RustError(error.to_string()))?,
        )?;
    }
    let status = response.status().as_u16();
    Ok(Response::from_bytes(response.into_body())?
        .with_status(status)
        .with_headers(headers))
}
