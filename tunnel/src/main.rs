mod cert;
mod client;
mod common;
mod conn_handler;
mod db;
mod dnsserver;
mod entity;
mod server;
mod sni_router;
mod timeout_stream;
mod web;

use std::{env::args, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    test: bool,
    command: String,
    directory: String,
    #[arg(long)]
    domain: Option<String>,
}

async fn make_env(args: &Args) -> Result<crate::common::Environment> {
    let env = if args.test {
        crate::common::Environment::test(args.directory.clone()).await?
    } else {
        crate::common::Environment::prod(args.directory.clone()).await?
    };
    Ok(env)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("help");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let args: Vec<_> = args().collect();
    println!("{:?}", args);

    let args = Args::parse();

    // let args: Vec<_> = args().collect();
    // let command = args.get(1).context("need an arg")?;
    if args.command == "server" {
        let env = make_env(&args).await?;
        let server_config = crate::common::read_json_file(&env.join_path("config.json")).await?;
        server::server_main(env, server_config).await?;
    } else if args.command == "client" {
        let env = make_env(&args).await?;
        let client_config = crate::common::read_json_file(&env.join_path("config.json")).await?;
        client::client_main(env, client_config).await?;
    } else if args.command == "test" {
        let env = make_env(&args).await?;
        let name = args.domain.context("need domain")?;
        let text = env.reqwest_client.get(name).send().await?.text().await?;
        print!("text: {text}");
    } else {
        bail!("unknown command {}", args.command);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello() -> Result<()> {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("help");

        tokio::process::Command::new("go")
            .args(&["build", "-o", "pebble", "./cmd/pebble"])
            .current_dir("./pebble")
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn")
            .wait()
            .await?;

        // let mut child = tokio::process::Command::new("go")
        // .args(&["run", "./cmd/pebble", "-dnsserver", "127.0.0.1:9999"])
        let mut child = tokio::process::Command::new("./pebble")
            .args(&["-dnsserver", "127.0.0.1:9999"])
            .current_dir("./pebble")
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn");

        tokio::time::sleep(Duration::from_secs(1)).await;

        // requires pebble to be running...
        // TODO: temp dir?
        tokio::fs::create_dir_all("./unittest").await?;
        let env = crate::common::Environment::test("./unittest".into()).await?;

        let db = crate::db::DB::new(&env).await?;

        // TODO: make this write to a special temp test directory instead
        let (dns_server_api, mut dns_server_internals) = dnsserver::make_dns_server(
            dnsserver::Authorizer::new(db.clone()).await?,
            db,
            "tunnel.jelle.dev.".into(),
            "0.0.0.0:9999".parse()?,
            "tunnel.jelle.dev:4444".into(),
            "tunnel.jelle.dev".into(),
        )
        .await?;
        tokio::spawn(async move {
            if let Err(err) = dns_server_internals.block_until_done().await {
                println!("dns server failed: {}", err);
            }
        });

        let hostname = "tunnel.jelle.dev";

        // TODO: put any limits on what records this client can write?
        let dns_client: Arc<Box<dyn crate::dnsserver::DnsClient>> =
            Arc::new(Box::new(dns_server_api.clone()));

        // TODO: put this maintainer in its own directory??
        let mut maintainer =
            cert::CertMaintainer::initialize(env.clone(), vec![hostname.into()], dns_client)
                .await?;

        let config = crate::common::make_rustls_server_config(maintainer.cert_resolver())?;

        tokio::spawn(async move { maintainer.maintain_certs().await });

        child.kill().await?;

        Ok(())
    }
}
