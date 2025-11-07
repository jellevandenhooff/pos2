use anyhow::Result;

// TODO: run continuously
// TODO: extract into wasi3experiment
// TODO: make the install script somehow wait for the initial download to finish (for exec cli -- maybe dockerloader itself can be the entrypoint and wait?) -- and what happens with the trial balloon?
// TODO: only copy (some) of the files into the loaded docker container?

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    dockerloader::init_dockerloaded().await?;

    // Check for intentional test failure
    if std::env::var("FAIL_INIT").is_ok_and(|v| v == "1") {
        anyhow::bail!("intentional failure for testing (FAIL_INIT=1)");
    }

    // Check for intentional test timeout
    if std::env::var("TIMEOUT_INIT").is_ok_and(|v| v == "1") {
        tracing::info!("TIMEOUT_INIT=1, sleeping for 60 seconds");
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }

    // Run the actual application logic
    let version = std::env::var("VERSION").unwrap_or_else(|_| "unknown".to_string());
    println!("Hello, world! Version: {}", version);

    // Mark ready - commits trial, does cleanup, checks for updates
    dockerloader::mark_ready().await?;

    // Simulate long-running process after marking ready
    if std::env::var("DOCKERLOADER_TRIAL").is_ok() {
        tracing::info!("trial mode: sleeping for 2 seconds to simulate long-running process");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    Ok(())
}
