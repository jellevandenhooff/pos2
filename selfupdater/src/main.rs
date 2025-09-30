use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use futures::stream::StreamExt;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

async fn check_for_new_image(docker: &Docker, repo: &str, channel: &str) -> Result<String> {
    let reference_string = format!("{}:{}", repo, channel);

    // TODO: this call is pretty slow... why?
    let image = docker
        .inspect_registry_image(&reference_string, None)
        .await?;

    let hash = image.descriptor.digest.context("missing digest")?;
    Ok(format!("{}@{}", repo, hash))
}

async fn pull_image(docker: &Docker, image: &str) -> Result<()> {
    info!("pulling image {}", image);

    let options = Some(bollard::query_parameters::CreateImageOptions {
        from_image: Some(image.into()), // can include tag
        ..Default::default()
    });
    let credentials = None;
    let mut output = docker.create_image(options, None, credentials.clone());
    while let Some(line) = output.next().await {
        match line {
            Ok(update) => info!("pulling: {:?}", update.progress_detail.unwrap()),
            Err(err) => {
                error!("error pulling image: {err}");
                bail!("Error pulling image: {err}");
            }
        }
    }

    Ok(())
}

async fn clean_up_images(docker: &Docker, repo: &str) -> Result<()> {
    let options = Some(bollard::query_parameters::ListImagesOptions {
        all: true,
        ..Default::default()
    });
    let images = docker.list_images(options).await?;

    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await?;

    let mut seen = HashSet::new();

    for container in containers {
        if let Some(id) = container.image_id {
            seen.insert(id);
        }
    }

    let prefix = format!("{}@", repo);
    for image in images {
        let mut all_good = true;
        let mut any = None;

        if seen.contains(&image.id) {
            continue;
        }

        for digest in image.repo_digests {
            if digest.starts_with(&prefix) {
                any = Some(digest.clone());
            } else {
                all_good = false;
            }
        }

        if let Some(digest) = any
            && all_good
        {
            // dangling image in our repo, delete it
            info!("deleting {}", digest);
            docker
                .remove_image(
                    &image.id,
                    Some(bollard::query_parameters::RemoveImageOptions {
                        ..Default::default()
                    }),
                    None,
                )
                .await?;
        }
    }
    Ok(())
}

async fn check_if_image_exists(docker: &Docker, reference: &str) -> Result<bool> {
    match docker.inspect_image(reference).await {
        Err(err) => {
            if let bollard::errors::Error::DockerResponseServerError {
                status_code,
                message: _,
            } = &err
                && status_code == &404
            {
                Ok(false)
            } else {
                bail!("failed to get {:?}", err);
            }
        }
        Ok(_) => Ok(true),
    }
}

struct Storage {
    pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
}

#[derive(Serialize, Deserialize, Debug)]
struct State {
    current_version: String, // for now, this is the image id.
    base_name: String,
    time_suffix: String,
    next_version: String,
    old_container_id: String,
    updater_container_id: String,
    new_container_id: String,
    status: Status,
    // TODO: last_change_at ???
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Status {
    None,
    PullingImage,
    StartingUpdater,
    StartedUpdater,
    StoppingOld,
    StartingNew,
    StartedNew,
    StoppingUpdaterAfterSuccess,
    StoppingNew,
    RestartingOld,
    StoppingUpdaterAfterFailure,
}

impl State {
    fn initial() -> Self {
        State {
            current_version: "".into(),
            base_name: "".into(),
            time_suffix: "".into(),
            next_version: "".into(),
            old_container_id: "".into(),
            updater_container_id: "".into(),
            new_container_id: "".into(),
            status: Status::None,
        }
    }
}

impl Storage {
    fn init() -> Result<Storage> {
        // let mut conn = rusqlite::Connection::open("/data/selfupdater.sqlite3")?;
        let manager = r2d2_sqlite::SqliteConnectionManager::file("/data/selfupdater.sqlite3");
        let pool = r2d2::Pool::new(manager)?;
        //
        let mut conn = pool.get()?;

        let tx = conn.transaction()?;

        if !tx.table_exists(Some("main"), "schema_version")? {
            tx.execute(
                "CREATE TABLE schema_version (version TEXT NOT NULL) STRICT",
                [],
            )?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?1)", ["v1"])?;
        }

        let mut version: String =
            tx.query_one("SELECT version FROM schema_version", [], |row| row.get(0))?;

        if version == "v1" {
            tx.execute("CREATE TABLE state (id TEXT PRIMARY KEY NOT NULL, state TEXT NOT NULL, version INTEGER NOT NULL) STRICT", [])?;
            tx.execute(
                "INSERT INTO state (id, state, version) VALUES (?1, ?2, ?3)",
                ("selfupdate", serde_json::to_string(&State::initial())?, 1),
            )?;
            version = "v2".into();
            tx.execute("UPDATE schema_version SET version = ?1", [&version])?;
        }

        tx.commit()?;

        Ok(Storage { pool: pool })
    }

    fn update_state(&self, new_state: &State, old_version: i64) -> Result<i64> {
        let changed = self.pool.get()?.execute(
            "UPDATE state SET version = ?1, state = ?2 WHERE id = ?3 and version = ?4",
            (
                old_version + 1,
                serde_json::to_string(new_state)?,
                "selfupdate",
                old_version,
            ),
        )?;
        if changed != 1 {
            bail!("state version mismatch; state changed by another process?");
        }
        info!("updated state to {:?}", new_state);
        Ok(old_version + 1)
    }

    fn get_state(&self) -> Result<(State, i64)> {
        let (state_str, version): (String, i64) = self.pool.get()?.query_one(
            "SELECT state, version FROM state WHERE id = ?1",
            ["selfupdate"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((serde_json::from_str(&state_str)?, version))
    }
}

// flow: create container, store id, start container
// if we crash between create container and store id... hopefully we can clean it up eventually with some background scan for containers
// TODO: logging options (for rotating logs?)

async fn create_updater_container(docker: &Docker, state: &State) -> Result<String> {
    let current_container = docker
        .inspect_container(
            &state.old_container_id,
            Some(bollard::query_parameters::InspectContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    let mut pretty_image = current_container.image.as_ref().unwrap().clone();
    let current_image = docker.inspect_image(&pretty_image).await?;
    if let Some(digests) = current_image.repo_digests {
        if digests.len() > 0 {
            pretty_image = digests[0].clone();
        }
    }

    let options = bollard::query_parameters::CreateContainerOptions {
        name: Some(format!("{}-{}-updater", state.base_name, state.time_suffix)),
        ..Default::default()
    };

    let body = bollard::models::ContainerCreateBody {
        image: Some(pretty_image),
        cmd: Some(vec!["/selfupdater".into(), "updater".into()]),
        host_config: Some(bollard::models::HostConfig {
            binds: current_container.host_config.unwrap().binds,
            //  Some(vec![
            //
            // ]),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::NO),
                maximum_retry_count: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = docker.create_container(Some(options), body).await?;
    Ok(created.id)
}

async fn create_new_container(docker: &Docker, state: &State) -> Result<String> {
    let current_container = docker
        .inspect_container(
            &state.old_container_id,
            Some(bollard::query_parameters::InspectContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    let options = bollard::query_parameters::CreateContainerOptions {
        name: Some(format!("{}-{}-new", state.base_name, state.time_suffix)),
        ..Default::default()
    };

    let body = bollard::models::ContainerCreateBody {
        image: Some(state.next_version.clone()),
        cmd: Some(vec!["/selfupdater".into()]), // TODO: copy from old?
        host_config: Some(bollard::models::HostConfig {
            binds: current_container.host_config.unwrap().binds,
            //  Some(vec![
            //
            // ]),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::NO),
                maximum_retry_count: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let created = docker.create_container(Some(options), body).await?;
    Ok(created.id)
}

async fn rename_container_idempotent(
    docker: &Docker,
    container_id: &str,
    name: &str,
) -> Result<()> {
    // TODO: check this is idempotent (and handles missing containers???)

    docker
        .rename_container(
            container_id,
            bollard::query_parameters::RenameContainerOptions { name: name.into() },
        )
        .await?;
    Ok(())
}

async fn stop_container_idempotent(docker: &Docker, container_id: &str) -> Result<()> {
    // TODO: check this is idempotent (and handles missing containers???)

    // FIXME: I think this update only works when the container is (still) running (???)
    docker
        .update_container(
            container_id,
            bollard::models::ContainerUpdateBody {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::NO),
                    maximum_retry_count: None,
                }),
                ..Default::default()
            },
        )
        .await?;

    docker
        .stop_container(
            container_id,
            Some(bollard::query_parameters::StopContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    Ok(())
}

async fn restart_container_idempotent(docker: &Docker, container_id: &str) -> Result<()> {
    // TODO: check this is idempotent
    docker
        .start_container(
            container_id,
            Some(bollard::query_parameters::StartContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    docker
        .update_container(
            container_id,
            bollard::models::ContainerUpdateBody {
                restart_policy: Some(bollard::models::RestartPolicy {
                    name: Some(bollard::models::RestartPolicyNameEnum::ALWAYS),
                    maximum_retry_count: None,
                }),
                ..Default::default()
            },
        )
        .await?;

    Ok(())
}

async fn identify_self(docker: &Docker) -> Result<bollard::models::ContainerSummary> {
    let options = Some(bollard::query_parameters::ListContainersOptions {
        all: true,
        filters: None, // Some(filters),
        ..Default::default()
    });

    let containers = docker.list_containers(options).await?;

    let uname = rustix::system::uname();
    let nodename = uname.nodename().to_str()?;

    let container = containers
        .into_iter()
        .filter(move |container| match &container.id {
            Some(id) => id.starts_with(&nodename),
            None => false,
        })
        .exactly_one()
        .or(Err(anyhow!("did not find exactly 1 container")))?;
    // .context("no container found")?;

    println!("container id: {:?}", container.id);
    println!("container name: {:?}", container.names);

    // docker.inspect_container(container_name, options)

    // let image_id = container.image_id.unwrap();
    Ok(container)
}

async fn consider_update(docker: &Docker, storage: &Storage, next_version: String) -> Result<()> {
    let (mut state, version) = storage.get_state()?;

    if state.status == Status::None
        && next_version != state.current_version
        && next_version != state.next_version
    {
        // TODO: check that we not previously failed on this specific version?
        let self_container = identify_self(&docker).await?;
        state.old_container_id = self_container.id.unwrap();
        state.new_container_id = "".into(); // TODO set these to empty when transitioning to NONE state?
        state.updater_container_id = "".into();

        state.time_suffix = chrono::Utc::now().format("%Y%m%dT%H%M").to_string();
        state.base_name = self_container
            .names
            .unwrap()
            .get(0)
            .unwrap()
            .strip_prefix("/")
            .unwrap()
            .into();
        state.status = Status::PullingImage;
        state.next_version = next_version;
        storage.update_state(&state, version)?;
        run_update_from_old(docker, storage).await?;
    }

    // TODO: stop doing work once we enter run_update_from_old (?)

    Ok(())
}

async fn update_loop_body(docker: &Docker, storage: &Storage) -> Result<()> {
    let repo = "localhost:5050/selfupdater";
    let next_version = check_for_new_image(docker, repo, "testing").await?;
    consider_update(docker, storage, next_version).await?;
    clean_up_images(docker, repo).await?;
    Ok(())
}

// TODO:
// - if we fail to make progress on a transition, eventually abort (???)
// - (gracefully) retry transitions
// - rename to old before spawning new
// - make sure we stop quickly when asked
// - (somehow) handle conflicts on compare-and-swap gracefully
// - (try to) write tests for the docker behavior
// - (briefly) write up the plan
// - end to end tests for multiple updates
// - wrap docker client in a type
// - hook up the health check
// - clean up old containers

async fn run_update_from_old(docker: &Docker, storage: &Storage) -> Result<()> {
    loop {
        let (mut state, mut version) = storage.get_state()?;
        match state.status {
            Status::PullingImage => {
                if !check_if_image_exists(docker, &state.next_version).await? {
                    pull_image(docker, &state.next_version).await?;
                }
                state.status = Status::StartingUpdater;
                storage.update_state(&state, version)?;
            }
            Status::StartingUpdater => {
                if state.updater_container_id == "" {
                    state.updater_container_id = create_updater_container(docker, &state).await?;
                    version = storage.update_state(&state, version)?;
                }
                restart_container_idempotent(docker, &state.updater_container_id).await?;
                state.status = Status::StartedUpdater;
                storage.update_state(&state, version)?;
            }
            Status::StartedUpdater => {
                // TODO: only do this after a timeout?
                state.status = Status::StoppingUpdaterAfterFailure;
                storage.update_state(&state, version)?;
            }
            Status::StoppingUpdaterAfterFailure => {
                stop_container_idempotent(docker, &state.updater_container_id).await?;
                rename_container_idempotent(docker, &state.old_container_id, &state.base_name)
                    .await?;
                state.status = Status::None; // TODO: track attempted version so we will not fail again?
                // TODO: check that all other containers are stopped?
                // TODO: check that container names make sense?
                storage.update_state(&state, version)?;
            }
            _ => tokio::time::sleep(tokio::time::Duration::from_secs(1)).await,
        }
    }

    // Ok(())
}

async fn run_update_from_updater(docker: &Docker, storage: &Storage) -> Result<()> {
    loop {
        let (mut state, mut version) = storage.get_state()?;
        match state.status {
            Status::StartingUpdater => {
                state.status = Status::StoppingOld;
                storage.update_state(&state, version)?;
            }
            Status::StoppingOld => {
                stop_container_idempotent(docker, &state.old_container_id).await?;
                rename_container_idempotent(
                    docker,
                    &state.old_container_id,
                    &format!("{}-{}-old", &state.base_name, &state.time_suffix),
                )
                .await?;
                state.status = Status::StartingNew;
                storage.update_state(&state, version)?;
            }
            Status::StartingNew => {
                if state.new_container_id == "" {
                    state.new_container_id = create_new_container(docker, &state).await?;
                    version = storage.update_state(&state, version)?;
                }
                restart_container_idempotent(docker, &state.new_container_id).await?;
                state.status = Status::StartedNew;
                storage.update_state(&state, version)?;
            }
            Status::StartedNew => {}
            Status::StoppingNew => {
                stop_container_idempotent(docker, &state.new_container_id).await?;
                state.status = Status::RestartingOld;
                storage.update_state(&state, version)?;
            }
            Status::RestartingOld => {
                restart_container_idempotent(docker, &state.old_container_id).await?;
                state.status = Status::StoppingUpdaterAfterFailure;
                storage.update_state(&state, version)?;
            }
            _ => tokio::time::sleep(tokio::time::Duration::from_secs(1)).await,
        }
    }

    // Ok(())
}

async fn run_update_from_new(docker: &Docker, storage: &Storage) -> Result<()> {
    loop {
        let (mut state, version) = storage.get_state()?;
        match state.status {
            Status::StartedNew => {
                let healthy = true;
                // do health check... then?
                if healthy {
                    state.status = Status::StoppingUpdaterAfterSuccess;
                } else {
                    state.status = Status::StoppingNew;
                }
                storage.update_state(&state, version)?;
            }
            Status::StoppingUpdaterAfterSuccess => {
                stop_container_idempotent(docker, &state.updater_container_id).await?;

                rename_container_idempotent(docker, &state.new_container_id, &state.base_name)
                    .await?;

                state.status = Status::None;
                storage.update_state(&state, version)?;

                return Ok(());
            }
            _ => tokio::time::sleep(tokio::time::Duration::from_secs(1)).await,
        }
    }

    // Ok(())
}

async fn cancellation_on_signal() -> Result<CancellationToken> {
    let cancellation = tokio_util::sync::CancellationToken::new();

    {
        let cancellation = cancellation.clone();
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

        tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.recv() => {
                    info!("received interrupt");
                }
                _ = terminate.recv() => {
                    info!("received quit");
                }
            }
            info!("received signal, stopping...");
            cancellation.cancel();
        });
    }

    Ok(cancellation)
}

#[tokio::main]
async fn main() -> Result<()> {
    let subscriber = tracing_subscriber::FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    let autoupdate_enabled = true; // TODO: make a flag?

    let cancellation = cancellation_on_signal().await?;

    let args: Vec<_> = std::env::args().collect();

    if args.get(1).is_some_and(|arg| arg == "updater") {
        let docker = Docker::connect_with_local_defaults()?;
        let storage = Storage::init()?;

        run_update_from_updater(&docker, &storage).await?;

        return Ok(());
    }

    if autoupdate_enabled {
        // do some sanity checks: only run if have access to docker socket, sqlite3, we know ourselves, and we are marked auto restart and not delete on exit

        let docker = Docker::connect_with_local_defaults()?;
        let self_container = identify_self(&docker).await?;
        let storage = Storage::init()?;
        let (state, _version) = storage.get_state()?;

        // TODO: if status is PullingImage, allow program to start???

        info!("read state {:?}", state);

        if state.status != Status::None {
            // some kind of update is running. figure out if we are the old or the new version and run appropriately?
            //
            // TODO: i think we can safely run "the app" below even while the update is running (except if we migrate stuff and we haven't yet committed to upgrading by passing a healthcheck??? tricky)
            if &state.old_container_id == self_container.id.as_ref().unwrap() {
                run_update_from_old(&docker, &storage).await?;
            } else if &state.new_container_id == self_container.id.as_ref().unwrap() {
                run_update_from_new(&docker, &storage).await?;
            } else {
                bail!("confusing state");
                // complain. we are running with a selfupdate state we don't recognize so maybe it's shared or something? should be cleaned up before we run.
                // declare we do not care about this update? kind of janky... this happens if somehow the db is stale or shared?
            }
        }

        // ok... now we are running successfully. run a checker in the background:
        tokio::spawn(async move {
            loop {
                if let Err(err) = update_loop_body(&docker, &storage).await {
                    error!("update loop error: {}", err);
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });
    }

    info!("i'm the newer version");

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

/*
async fn get_container_ip(docker: &Docker, container: &str) -> Result<String> {
    let inspection = docker
        .inspect_container(
            &container,
            Some(bollard::query_parameters::InspectContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    let network_settings = inspection
        .network_settings
        .context("missing network settings")?;
    let networks = network_settings.networks.context("missing networks")?;
    for (_name, network) in networks {
        if let Some(address) = network.ip_address {
            return Ok(address);
        }
    }
    bail!("no ip addresses found");
}

async fn health_check(docker: &Docker, container: &str) -> Result<()> {
    let address = get_container_ip(docker, container).await?;

    let url = format!("http://{}:8080/healthcheck", address,);
    info!("requesting {}", url);

    // TODO: have a timeout here, and retry?

    let resp = reqwest::get(url).await?.text().await?;
    info!("got: {}", resp);

    let trimmed = resp.trim();

    if trimmed == "OK" {
        Ok(())
    } else {
        bail!("did not get OK response: {}", trimmed)
    }
}

tokio::spawn(async move {
    async fn hello_world() -> &'static str {
        "not OK"
    }

    let router = axum::Router::new().route("/healthcheck", axum::routing::get(hello_world));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 8080));
    let tcp = tokio::net::TcpListener::bind(&addr).await.unwrap();

    axum::serve(tcp, router).await.unwrap();
});
*/
