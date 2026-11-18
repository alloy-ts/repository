//! MCP routes for the local Axum server.
//!
//! Keep protocol handlers in `McpRouter`; this module only mounts transports.
//! A caller can compose MCP tools with `McpRouter::nest` and `merge`, configure
//! per-tool layers on `ToolBuilder`, configure each transport with `http` or
//! `websocket`, then apply HTTP middleware to the returned Axum router.

use axum::Router;
use tower_mcp::{HttpTransport, McpRouter, WebSocketTransport};

use super::server::Server;

/// Mounts the same MCP tool set over streamable HTTP and WebSocket.
pub struct McpServer {
    http: HttpTransport,
    websocket: WebSocketTransport,
}

impl McpServer {
    pub fn new(router: McpRouter) -> Self {
        Self {
            http: HttpTransport::new(router.clone()),
            websocket: WebSocketTransport::new(router),
        }
    }

    /// Configure transport-wide middleware or HTTP session settings.
    pub fn http(mut self, configure: impl FnOnce(HttpTransport) -> HttpTransport) -> Self {
        self.http = configure(self.http);
        self
    }

    /// Configure transport-wide middleware or WebSocket settings.
    pub fn websocket(
        mut self,
        configure: impl FnOnce(WebSocketTransport) -> WebSocketTransport,
    ) -> Self {
        self.websocket = configure(self.websocket);
        self
    }

    /// Build an ordinary Axum router. Apply HTTP middleware with `.layer(...)`.
    /// Streamable HTTP is at `/mcp`; WebSocket is at `/mcp/ws`.
    pub fn into_router(self) -> Router {
        Router::new()
            .nest("/mcp/ws", self.websocket.into_router())
            .nest("/mcp", self.http.into_router())
    }
}

impl Server {
    /// Serve the existing local application together with MCP transports.
    pub async fn start_with_mcp(&self, mcp: McpServer) -> napi::Result<u16> {
        self.start_with_router(mcp.into_router()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn both_transports_are_mounted() {
        let router = McpRouter::new().server_info("alloy", "0.1.0");
        let app = McpServer::new(router).into_router();
        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/mcp/health")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let ws = app
            .oneshot(
                Request::builder()
                    .uri("/mcp/ws")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ws.status(), StatusCode::BAD_REQUEST);
    }
}
