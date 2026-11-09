//! Web Fetch API implementation using `http_request`.
//!
//! Exposes `fetch(input, init)` compatible with MDN Web Fetch API:
//! const response = await fetch("http://example.com");
//! const response = await fetch("https://example.com");
//! const response = await fetch(request);

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::net::request::{AbortFlag, Request, RequestInit, Response};

/// Fetch a resource from the network.
///
/// Accepts a URL string or a `Request` instance, and optional `RequestInit`.
/// Returns a Promise that resolves to a `Response` object.
/// `abort` rejects the operation when {@link AbortFlag.abort} has been called.
#[napi]
pub async fn fetch(
    input: Either<String, &Request>,
    init: Option<RequestInit>,
    abort: Option<&AbortFlag>,
) -> napi::Result<Response> {
    if abort.as_ref().is_some_and(|flag| flag.is_aborted()) {
        return Err(napi::Error::from_reason("The operation was aborted"));
    }
    let request = Request::new(input, init)?;
    request.start(abort).await
}

/// Result of a remote git network handshake or ls-refs query.
#[napi(object)]
pub struct NetworkFetchHandshake {
    pub protocol_version: String,
    pub capabilities: Vec<String>,
}

/// Query remote git capabilities over the wire.
#[napi]
pub fn query_remote_capabilities(
    capabilities_banner: String,
) -> napi::Result<NetworkFetchHandshake> {
    let caps = crate::net::protocol::parse_capabilities(capabilities_banner)?;
    Ok(NetworkFetchHandshake {
        protocol_version: "V2".to_string(),
        capabilities: caps,
    })
}
