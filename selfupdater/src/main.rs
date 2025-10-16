use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = tracing_subscriber::FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    let autoupdate_enabled = true; // TODO: make a flag?

    let cancellation = selfupdater::cancellation_on_signal().await?;

    let args: Vec<_> = std::env::args().collect();

    let updater_config = selfupdater::UpdaterConfiguration {
        repository: "localhost:5050/selfupdater".into(),
        tag: "testing".into(),
        delete_dangling_created_containers_after: chrono::Duration::seconds(30),
        delete_unused_containers_after: chrono::Duration::seconds(30),
        delete_unused_images_pulled_before: chrono::Duration::seconds(30),
    };

    if args.get(1).is_some_and(|cmd| cmd == "selfupdater-helper") {
        let updater = selfupdater::SelfUpdater::new(updater_config).await?;
        updater.run_helper(cancellation.clone()).await?;
        return Ok(());
    }

    if autoupdate_enabled {
        let updater = selfupdater::SelfUpdater::new(updater_config).await?;
        updater.start_main(cancellation.clone()).await?;
    }

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
