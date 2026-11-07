//! Node HTTP compatibility is exported by main.ts using node:http.
//! The previous raw-TCP implementation ignored request listeners and always
//! returned a fixed body. Use the public HttpServer/createHttpServer adapters
//! for Node streams/events, or net::server::Server for the Rocket Web API.
