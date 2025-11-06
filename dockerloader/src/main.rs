use anyhow::Result;
use std::ffi::CString;

fn execve_entrypoint() -> Result<std::convert::Infallible> {
    let entrypoint = CString::new(dockerloader::ENTRYPOINT_PATH)?;

    // Pass entrypoint as argv[0]
    let args = vec![entrypoint.clone()];

    // Pass through current environment variables
    let env: Vec<CString> = std::env::vars()
        .map(|(key, value)| CString::new(format!("{}={}", key, value)))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(nix::unistd::execve(&entrypoint, &args, &env)?)
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
