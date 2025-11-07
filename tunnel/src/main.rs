use std::env::args;

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

async fn make_env(args: &Args) -> Result<tunnel::common::Environment> {
    let env = if args.test {
        tunnel::common::Environment::test(args.directory.clone()).await?
    } else {
        tunnel::common::Environment::prod(args.directory.clone()).await?
    };
    Ok(env)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("help");

    // tokio::time::sleep(Duration::from_millis(100)).await;

    let args: Vec<_> = args().collect();
    println!("{:?}", args);

    let args = Args::parse();

    // let args: Vec<_> = args().collect();
    // let command = args.get(1).context("need an arg")?;
    if args.command == "server" {
        let env = make_env(&args).await?;
        let server_config = tunnel::common::read_json_file(&env.join_path("config.json")).await?;
        tunnel::server::server_main(env, server_config).await?;
    } else if args.command == "client" {
        let env = make_env(&args).await?;
        let client_config = tunnel::common::read_json_file(&env.join_path("config.json")).await?;
        tunnel::client::client_main(env, tunnel::client::test_conn_handler(), client_config)
            .await?;
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
