use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    if !tokio::fs::try_exists("/data").await? {
        tracing::error!("missing /data directory; make sure it is mounted as a volume");
        return Ok(());
    }

    tracing::info!(
        "started dockerloader, checking for {} ",
        dockerloader::ENTRYPOINT_PATH
    );

    // Download entrypoint if missing
    if !tokio::fs::try_exists(dockerloader::ENTRYPOINT_PATH).await? {
        let reference = std::env::var("DOCKERLOADER_TARGET")?;
        tracing::info!("entrypoint missing, downloading {}", reference);
        dockerloader::download_entrypoint_initial(&reference).await?;
    }

    loop {
        // Check for pending update attempt
        let in_trial_mode =
            if tokio::fs::try_exists(dockerloader::ENTRYPOINT_ATTEMPT_PATH).await? {
                tracing::info!(
                    "found entrypoint-attempt, renaming to entrypoint-attempting for trial"
                );
                tokio::fs::rename(
                    dockerloader::ENTRYPOINT_ATTEMPT_PATH,
                    dockerloader::ENTRYPOINT_ATTEMPTING_PATH,
                )
                .await
                .context("failed to rename entrypoint-attempt to entrypoint-attempting")?;
                true
            } else {
                false
            };

        let entrypoint_to_spawn = if in_trial_mode {
            dockerloader::ENTRYPOINT_ATTEMPTING_PATH
        } else {
            dockerloader::ENTRYPOINT_PATH
        };

        let symlink_target = tokio::fs::read_link(entrypoint_to_spawn)
            .await
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string());

        tracing::info!(
            "spawning entrypoint {} -> {} as subprocess (trial mode: {})",
            entrypoint_to_spawn,
            symlink_target,
            in_trial_mode
        );

        let status = if in_trial_mode {
            let timeout_duration = std::env::var("DOCKERLOADER_TRIAL_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_millis)
                .unwrap_or(std::time::Duration::from_secs(10));

            let mut child = tokio::process::Command::new(entrypoint_to_spawn)
                .envs(std::env::vars())
                .env("DOCKERLOADER_TRIAL", "1")
                .spawn()
                .context("failed to spawn entrypoint")?;

            tokio::select! {
                result = child.wait() => {
                    result.context("failed to wait for entrypoint")?
                }
                _ = tokio::time::sleep(timeout_duration) => {
                    // Timeout hit, check if trial was committed
                    if tokio::fs::try_exists(dockerloader::ENTRYPOINT_ATTEMPTING_PATH).await? {
                        // Trial not committed, kill child
                        child.kill().await.ok();
                        anyhow::bail!(
                            "DOCKERLOADER_TRIAL timeout hit after {:#?}, aborting",
                            timeout_duration
                        );
                    } else {
                        // Trial was committed, wait for child to exit normally
                        tracing::info!(
                            "trial committed within timeout, subprocess can continue running"
                        );
                        child.wait().await.context("failed to wait for entrypoint")?
                    }
                }
            }
        } else {
            tokio::process::Command::new(entrypoint_to_spawn)
                .envs(std::env::vars())
                .status()
                .await
                .context("failed to spawn entrypoint")?
        };

        let exit_code = status.code().unwrap_or(1);

        if exit_code == dockerloader::RESTART_EXIT_CODE {
            tracing::info!("subprocess exited with code 42, restarting");
            continue;
        }

        std::process::exit(exit_code);
    }
}
