//! Runtime-independent buffered HTTP contract. No N-API, Tokio, filesystem or sockets.
//! Runtime adapters own invocation, limits and platform-specific context.
pub use http;
pub type Request = http::Request<Vec<u8>>;
pub type Response = http::Response<Vec<u8>>;

pub fn is_transport_header(name: &str) -> bool {
    ["content-length", "transfer-encoding", "connection"]
        .iter()
        .any(|header| name.eq_ignore_ascii_case(header))
}

/// Remove transport framing and bodies forbidden by HTTP semantics.
pub fn prepare_response(mut response: Response, head: bool) -> Response {
    for name in ["content-length", "transfer-encoding", "connection"] {
        response.headers_mut().remove(name);
    }
    let status = response.status();
    if head || status.is_informational() || matches!(status.as_u16(), 204 | 205 | 304) {
        response.body_mut().clear();
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_binary_and_duplicate_cookies_but_not_transport_headers() {
        let response = http::Response::builder()
            .status(201)
            .header("set-cookie", "a=1")
            .header("set-cookie", "b=2")
            .header("content-length", "999")
            .body(vec![0, 255])
            .unwrap();
        let response = prepare_response(response, false);
        assert_eq!(response.body(), &[0, 255]);
        assert_eq!(response.headers().get_all("set-cookie").iter().count(), 2);
        assert!(!response.headers().contains_key("content-length"));
    }
    #[test]
    fn suppresses_head_and_no_content_bodies() {
        assert!(
            prepare_response(Response::new(vec![1]), true)
                .body()
                .is_empty()
        );
        let response = http::Response::builder().status(204).body(vec![1]).unwrap();
        assert!(prepare_response(response, false).body().is_empty());
    }
}
