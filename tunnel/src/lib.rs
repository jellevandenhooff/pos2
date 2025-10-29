use anyhow::Result;

pub mod cert;
pub mod client;
pub mod common;
mod conn_handler;
pub mod db;
pub mod dnsserver;
mod entity;
pub mod server;
mod sni_router;
mod timeout_stream;
mod web;

pub async fn run_client(directory: String, router: axum::Router<()>) -> Result<()> {
    let env = common::Environment::prod(directory).await?;
    let client_config = common::read_json_file(&env.join_path("config.json")).await?;
    let handler = conn_handler::axum_router_to_conn_handler(router);
    client::client_main(env, handler, client_config).await?;
    Ok(())
}
