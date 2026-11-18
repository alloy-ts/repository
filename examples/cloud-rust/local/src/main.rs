#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse::<u16>()
        .expect("PORT must be a u16");
    alloy_server_local::serve(
        alloy_server_local::Options {
            address: std::net::Ipv4Addr::UNSPECIFIED.into(),
            port,
            ..Default::default()
        },
        cloud_example_app::handle,
    )
    .await
}
