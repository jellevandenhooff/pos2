use anyhow::{Context, Result};

fn execve_entrypoint() -> Result<std::convert::Infallible> {
    // Resolve symlink to get actual binary path
    let real_path = std::fs::canonicalize(dockerloader::ENTRYPOINT_PATH)
        .context("failed to resolve entrypoint symlink")?;
    tracing::info!("resolved entrypoint to: {}", real_path.display());

    let real_path_str = real_path
        .to_str()
        .context("entrypoint path not valid utf-8")?;

    // Sleep briefly to avoid race condition with Docker filesystem sync
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Pass through current environment variables
    let env: Vec<(String, String)> = std::env::vars().collect();

    dockerloader::execve_into(real_path_str, &env)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    tracing::info!(
        "started dockerloader, checking for {}",
        dockerloader::ENTRYPOINT_PATH
    );
    if tokio::fs::try_exists(dockerloader::ENTRYPOINT_PATH).await? {
        tracing::info!(
            "entrypoint {} exists, invoking it",
            dockerloader::ENTRYPOINT_PATH
        );
        execve_entrypoint()?;
    }

    // TODO: get from environment?
    // let reference = "ghcr.io/jellevandenhooff/pos2:main";
    let reference = "host.docker.internal:5050/dockerloaded:testing";

    tracing::info!("entrypoint missing, downloading {}", reference);
    dockerloader::download_entrypoint_initial(reference).await?;

    tracing::info!(
        "invoking entrypoint {} after download",
        dockerloader::ENTRYPOINT_PATH
    );
    execve_entrypoint()?;
    Ok(())
}
