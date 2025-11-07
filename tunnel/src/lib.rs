use anyhow::Result;

pub mod cert;
pub mod client;
pub mod common;
mod conn_handler;
pub mod db;
pub mod dnsserver;
mod entity;
pub mod server;
pub mod setup;
mod sni_router;
mod timeout_stream;
pub mod web;

pub async fn run_client(directory: String, router: axum::Router<()>) -> Result<()> {
    let env = common::Environment::prod(directory).await?;
    let client_config = common::read_optional_json_file(&env.join_path("config.json"))
        .await?
        .unwrap_or_else(|| client::ClientConfig {
            endpoints: client::DEFAULT_ENDPOINTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    let handler = conn_handler::axum_router_to_conn_handler(router);
    client::client_main(env, handler, client_config).await?;
    Ok(())
}
