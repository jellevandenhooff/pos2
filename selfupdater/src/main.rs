use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = tracing_subscriber::FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    let cancellation = selfupdater::cancellation_on_signal().await?;

    selfupdater::run(
        selfupdater::UpdaterConfiguration {
            repository: "localhost:5050/selfupdater".into(),
            tag: "testing".into(),
            delete_dangling_created_containers_after: chrono::Duration::seconds(30),
            delete_unused_containers_after: chrono::Duration::seconds(30),
            delete_unused_images_pulled_before: chrono::Duration::seconds(30),
        },
        cancellation.clone(),
    )
    .await?;

    info!("i'm the newerer version");
    if let Ok(version) = std::env::var("TEST_VERSION")
        && version != ""
    {
        info!("version from environment: {}", version);
    }

    loop {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                info!("tick");
            }
            _ = cancellation.cancelled() => {
                info!("stopping the app");
                break
            }
        }
    }

    Ok(())
}
