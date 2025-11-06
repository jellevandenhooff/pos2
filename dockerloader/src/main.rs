use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    if !tokio::fs::try_exists("/data").await? {
        tracing::error!("missing /data directory; make sure it is mounted as a volume");
        return Ok(());
    }

    tracing::info!(
        "started dockerloader, checking for {}",
        dockerloader::ENTRYPOINT_PATH
    );
    if tokio::fs::try_exists(dockerloader::ENTRYPOINT_PATH).await? {
        tracing::info!(
            "entrypoint {} exists, invoking it",
            dockerloader::ENTRYPOINT_PATH
        );
        dockerloader::execve_into(
            std::path::Path::new(dockerloader::ENTRYPOINT_PATH),
            &std::env::vars().collect(),
        )?;
    }

    let reference = std::env::var("DOCKERLOADER_TARGET")?;
    tracing::info!("entrypoint missing, downloading {}", reference);
    dockerloader::download_entrypoint_initial(&reference).await?;

    tracing::info!(
        "invoking entrypoint {} after download",
        dockerloader::ENTRYPOINT_PATH
    );
    dockerloader::execve_into(
        std::path::Path::new(dockerloader::ENTRYPOINT_PATH),
        &std::env::vars().collect(),
    )?;
    Ok(())
}
