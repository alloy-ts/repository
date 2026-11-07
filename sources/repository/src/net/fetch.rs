//! Web Fetch API implementation using `http_request`.
//!
//! Exposes `fetch(input, init)` compatible with MDN Web Fetch API:
//! const response = await fetch("http://example.com");
//! const response = await fetch("https://example.com");
//! const response = await fetch(request);

use std::collections::HashMap;
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::net::request::{execute_http_request, Request, RequestInit, Response};

/// Fetch a resource from the network.
///
/// Accepts a URL string or a `Request` instance, and optional `RequestInit`.
/// Returns a Promise that resolves to a `Response` object.
#[napi]
pub async fn fetch(
    input: Either<String, &Request>,
    init: Option<RequestInit>,
) -> napi::Result<Response> {
    let mut init = init.unwrap_or_default();
    let (url, default_method, default_headers, default_body) = match input {
        Either::A(u) => (u, "GET".to_string(), HashMap::new(), None),
        Either::B(req) => (
            req.url(),
            req.method(),
            req.headers(),
            match req.array_buffer().await {
                Ok(buf) if !buf.is_empty() => Some(buf.to_vec()),
                _ => None,
            },
        ),
    };

    let method = init
        .method
        .take()
        .unwrap_or(default_method)
        .to_uppercase();

    let mut headers = default_headers;
    if let Some(h) = init.headers.take() {
        for (k, v) in h {
            headers.insert(k.to_lowercase(), v);
        }
    }

    let body = match init.body.take() {
        Some(Either::A(s)) => Some(s.into_bytes()),
        Some(Either::B(b)) => Some(b.to_vec()),
        None => default_body,
    };

    execute_http_request(
        &method,
        &url,
        &headers,
        body.as_deref(),
    )
    .await
}

/// Result of a remote git network handshake or ls-refs query.
#[napi(object)]
pub struct NetworkFetchHandshake {
    pub protocol_version: String,
    pub capabilities: Vec<String>,
}

/// Query remote git capabilities over the wire.
#[napi]
pub fn query_remote_capabilities(capabilities_banner: String) -> napi::Result<NetworkFetchHandshake> {
    let caps = crate::net::protocol::parse_capabilities(capabilities_banner)?;
    Ok(NetworkFetchHandshake {
        protocol_version: "V2".to_string(),
        capabilities: caps,
    })
}
