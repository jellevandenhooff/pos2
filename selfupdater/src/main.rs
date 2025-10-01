use std::collections::HashSet;

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
            Ok(update) => info!("pulling: {:?}", update),
            Err(err) => {
                error!("error pulling image: {err}");
                bail!("error pulling image: {err}");
            }
        }
    }

    Ok(())
}

async fn clean_up_images_and_containers(docker: &Docker, repo: &str) -> Result<()> {
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

    let prefix = format!("{}@", repo);

    for container in containers {
        // TODO: clean up this loop's code
        // TODO: identify (our) containers some other way besides checking the image??? check the name as well???
        // TODO: maybe store containers (and images) we have created and/or pulled in our state and only delete those?
        if container.state == Some(bollard::models::ContainerSummaryStateEnum::EXITED) {
            if container
                .image
                .is_some_and(|image| image.starts_with(&prefix))
            {
                let inspected = docker
                    .inspect_container(
                        container.id.as_ref().unwrap(),
                        Some(bollard::query_parameters::InspectContainerOptions {
                            ..Default::default()
                        }),
                    )
                    .await?;

                let state = inspected.state.unwrap();
                let finished_at =
                    chrono::DateTime::parse_from_rfc3339(&state.finished_at.unwrap())?.to_utc();
                if finished_at + chrono::Duration::minutes(5) <= chrono::Utc::now() {
                    info!(
                        "deleting old container {:?} {:?}",
                        container.id, container.names
                    );

                    docker
                        .remove_container(
                            &container.id.unwrap(),
                            Some(bollard::query_parameters::RemoveContainerOptions {
                                ..Default::default()
                            }),
                        )
                        .await?;
                    // don't mark this image as in-use
                    continue;
                }
            }
        }

        if let Some(id) = container.image_id {
            seen.insert(id);
        }
    }

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
    last_change_at: Option<chrono::DateTime<chrono::Utc>>,
}

// flow: create container, store id, start container
// if we crash between create container and store id... hopefully we can clean it up eventually with some background scan for containers

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Status {
    None,
    PullingImage,
    CreatingUpdater,
    StartingUpdater,
    StartedUpdater,
    StoppingOld,
    CreatingNew,
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
            last_change_at: None,
        }
    }
}

impl Storage {
    fn init() -> Result<Storage> {
        // TODO: wal? other flags?
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

    // TODO: janky. this is here because if an image gets untagged the above pretty image causes problems (???)
    if !check_if_image_exists(docker, &pretty_image).await? {
        pull_image(&docker, &pretty_image).await?;
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
        name: Some(format!(
            "{}-{}-new-pending",
            state.base_name, state.time_suffix
        )),
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
    let current = docker
        .inspect_container(
            &container_id,
            Some(bollard::query_parameters::InspectContainerOptions {
                ..Default::default()
            }),
        )
        .await?;
    if current.name.unwrap().trim_start_matches("/") == name {
        return Ok(());
    }
    docker
        .rename_container(
            container_id,
            bollard::query_parameters::RenameContainerOptions { name: name.into() },
        )
        .await?;
    Ok(())
}

async fn stop_container_idempotent(docker: &Docker, container_id: &str) -> Result<()> {
    // TODO: think about missing containers?
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

async fn start_container_idempotent(docker: &Docker, container_id: &str) -> Result<()> {
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

    println!("container id: {:?}", container.id);
    println!("container name: {:?}", container.names);

    Ok(container)
}

async fn consider_update(
    docker: &Docker,
    storage: &Storage,
    next_version: String,
    cancellation: CancellationToken,
) -> Result<()> {
    let (mut state, version) = storage.get_state()?;
    // TODO: if current version is none, initialize to current container version?
    // TODO: set old container id to self if empty? if not empty, complain? (maybe allow resetting if it mentions a non-existent container?)

    if state.status == Status::None
        && next_version != state.current_version
        && next_version != state.next_version
    {
        let self_container = identify_self(&docker).await?;
        state.old_container_id = self_container.id.unwrap();
        state.new_container_id = "".into(); // TODO set these to empty when transitioning to NONE state?
        state.updater_container_id = "".into();

        state.time_suffix = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
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
        state.last_change_at = Some(chrono::Utc::now());
        storage.update_state(&state, version)?;
        run_update(docker, storage, update_from_old_step, cancellation).await?;
    }

    // TODO: maybe stop doing work in the main process once we enter run_update_from_old (?)

    Ok(())
}

async fn update_loop_body(
    docker: &Docker,
    storage: &Storage,
    cancellation: CancellationToken,
) -> Result<()> {
    let repo = "localhost:5050/selfupdater";
    let next_version = check_for_new_image(docker, repo, "testing").await?;
    consider_update(docker, storage, next_version, cancellation).await?;
    clean_up_images_and_containers(docker, repo).await?;
    Ok(())
}

// TODO:
// - (briefly) write up the plan
// - end to end tests for multiple updates
// - log what we are doing
// - hook up the health check
// - fun end to end test with injected failure before/after every step
// - store base name in env var (or label)
// - logging options (for rotating logs?)
// - timeouts on docker API calls / external operations?
// - ensure running container always has simple name??

enum StepResult {
    ShutdownProcess,
    Transition(State),
    Sleep,
    Retry,
    StateMachineDone,
}

async fn run_update(
    docker: &Docker,
    storage: &Storage,
    step_func: impl AsyncFn(&Docker, State) -> StepResult,
    cancellation_token: CancellationToken,
) -> Result<()> {
    loop {
        let (state, old_version) = storage.get_state()?;
        match step_func(docker, state).await {
            StepResult::ShutdownProcess => {
                // TODO: this leads to immediately restarting because of the restart policy... instead, just handle the cancellation token better?
                // do not shut ourselves down, but wait for a signal (to prevent immediate restart with the policy)
                cancellation_token.cancelled().await;
                std::process::exit(0);
            }
            StepResult::Transition(mut new_state) => {
                new_state.last_change_at = Some(chrono::Utc::now());
                match storage.update_state(&new_state, old_version) {
                    Ok(_) => {
                        continue;
                    }
                    Err(err) => {
                        error!("failed to do update state: {}; will wait and retry", err);
                        // TODO: exponential back-off? bound number of failures?
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                }
            }
            StepResult::Retry => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            StepResult::Sleep => {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            StepResult::StateMachineDone => return Ok(()),
        }
    }
}

async fn update_from_old_step(docker: &Docker, mut state: State) -> StepResult {
    match state.status {
        Status::PullingImage => {
            if let Err(err) = pull_image(docker, &state.next_version).await {
                // TODO: retry a couple of times?
                error!("failed to pull image: {}; will rollback", err);
                state.status = Status::StoppingUpdaterAfterFailure;
                return StepResult::Transition(state);
            }
            state.status = Status::CreatingUpdater;
            StepResult::Transition(state)
        }
        Status::CreatingUpdater => {
            match create_updater_container(docker, &state).await {
                Ok(id) => {
                    state.updater_container_id = id;
                }
                Err(err) => {
                    // TODO: retry a couple of times?
                    error!("failed to create updater: {}; will rollback", err);
                    state.status = Status::StoppingUpdaterAfterFailure;
                    return StepResult::Transition(state);
                }
            }
            state.status = Status::StartingUpdater;
            StepResult::Transition(state)
        }
        Status::StartingUpdater => {
            if let Err(err) = start_container_idempotent(docker, &state.updater_container_id).await
            {
                // TODO: retry a couple of times?
                error!("failed to start updater: {}", err);
                state.status = Status::StoppingUpdaterAfterFailure;
                return StepResult::Transition(state);
            }
            state.status = Status::StartedUpdater;
            StepResult::Transition(state)
        }
        Status::StartedUpdater => {
            // TODO: more elegant timeout somehow?
            if state.last_change_at.is_some_and(|last_change| {
                chrono::Utc::now() >= last_change + chrono::Duration::seconds(10)
            }) {
                state.status = Status::StoppingUpdaterAfterFailure;
                StepResult::Transition(state)
            } else {
                StepResult::Sleep
            }
        }
        Status::StoppingOld => StepResult::ShutdownProcess,
        Status::StoppingUpdaterAfterFailure => {
            if state.updater_container_id != "" {
                if let Err(err) =
                    stop_container_idempotent(docker, &state.updater_container_id).await
                {
                    // TODO: ignore if missing?
                    error!("failed to stop updater: {}; will retry", err);
                    return StepResult::Retry;
                }
                if let Err(err) = rename_container_idempotent(
                    docker,
                    &state.updater_container_id,
                    &format!("{}-{}-updater-failed", &state.base_name, &state.time_suffix),
                )
                .await
                {
                    // TODO: ignore if this keeps failing?
                    error!("failed to rename updater: {}; will retry", err);
                    return StepResult::Retry;
                }
            }
            // TODO: only if container exists?
            state.status = Status::None; // TODO: track attempted version so we will not fail again?
            state.current_version = state.next_version;
            state.next_version = "".into();
            state.old_container_id = "".into();
            state.updater_container_id = "".into();
            state.new_container_id = "".into();
            state.base_name = "".into();
            state.time_suffix = "".into();
            // TODO: check that all other containers are stopped?
            // TODO: check that container names make sense?
            StepResult::Transition(state)
        }
        Status::None => StepResult::StateMachineDone,
        _ => StepResult::Sleep,
    }
}

async fn update_from_updater_step(docker: &Docker, mut state: State) -> StepResult {
    match state.status {
        Status::StartedUpdater => {
            state.status = Status::StoppingOld;
            StepResult::Transition(state)
            // on failure, we will be stopped by old
        }
        Status::StoppingOld => {
            if let Err(err) = stop_container_idempotent(docker, &state.old_container_id).await {
                // TODO: retry a couple of times?
                error!("failed to stop old: {}; will rollback", err);
                state.status = Status::RestartingOld;
                return StepResult::Transition(state);
            }
            if let Err(err) = rename_container_idempotent(
                docker,
                &state.old_container_id,
                &format!("{}-{}-old-pending", &state.base_name, &state.time_suffix),
            )
            .await
            {
                // TODO: retry a couple of times?
                error!("failed to rename old: {}; will rollback", err);
                state.status = Status::RestartingOld;
                return StepResult::Transition(state);
            }
            state.status = Status::CreatingNew;
            StepResult::Transition(state)
        }
        Status::CreatingNew => {
            match create_new_container(docker, &state).await {
                Ok(id) => {
                    state.new_container_id = id;
                }
                Err(err) => {
                    // TODO: retry a couple of times?
                    error!("failed to create new: {}; will rollback", err);
                    state.status = Status::RestartingOld;
                    return StepResult::Transition(state);
                }
            }
            state.status = Status::StartingNew;
            // TODO: on failure, go to stopping new (and make stopping new tolerate missing container)
            StepResult::Transition(state)
        }
        Status::StartingNew => {
            if let Err(err) =
                rename_container_idempotent(docker, &state.new_container_id, &state.base_name).await
            {
                error!("failed to rename new: {}; will rollback", err);
                state.status = Status::StoppingNew;
                return StepResult::Transition(state);
            }
            if let Err(err) = start_container_idempotent(docker, &state.new_container_id).await {
                // TODO: retry a couple of times?
                error!("failed to start new: {}; will rollback", err);
                state.status = Status::StoppingNew;
                return StepResult::Transition(state);
            }
            state.status = Status::StartedNew;
            StepResult::Transition(state)
        }
        Status::StartedNew => {
            if state.last_change_at.is_some_and(|last_change| {
                chrono::Utc::now() >= last_change + chrono::Duration::seconds(10)
            }) {
                state.status = Status::StoppingNew;
                StepResult::Transition(state)
            } else {
                StepResult::Sleep
            }
            // this is the failure handler
        }
        Status::StoppingNew => {
            if state.new_container_id != "" {
                if let Err(err) = stop_container_idempotent(docker, &state.new_container_id).await {
                    // TODO: ignore if missing?
                    error!("failed to stop new: {}; will retry", err);
                    return StepResult::Retry;
                }
                if let Err(err) = rename_container_idempotent(
                    docker,
                    &state.new_container_id,
                    &format!("{}-{}-new-failed", &state.base_name, &state.time_suffix),
                )
                .await
                {
                    // TODO: ignore if missing?
                    error!("failed to rename old: {}; will retry", err);
                    return StepResult::Retry;
                }
            }
            state.status = Status::RestartingOld;
            StepResult::Transition(state)
        }
        Status::RestartingOld => {
            if let Err(err) =
                rename_container_idempotent(docker, &state.old_container_id, &state.base_name).await
            {
                error!("failed to rename old: {}; will retry", err);
                return StepResult::Retry;
            }
            if let Err(err) = start_container_idempotent(docker, &state.old_container_id).await {
                error!("failed to start old: {}; will retry", err);
                return StepResult::Retry;
            }
            state.status = Status::StoppingUpdaterAfterFailure;
            StepResult::Transition(state)
            // cannot fail (hopefully)
        }
        Status::StoppingUpdaterAfterSuccess | Status::StoppingUpdaterAfterFailure => {
            StepResult::ShutdownProcess
        }
        _ => StepResult::Sleep,
    }
}

async fn update_from_new_step(docker: &Docker, mut state: State) -> StepResult {
    match state.status {
        Status::StartedNew => {
            // configurable with env var for testing...
            let mut healthy = true;
            if let Ok(value) = std::env::var("TEST_HEALTHY")
                && value != ""
            {
                healthy = value.parse().unwrap_or(false); // .ok().on_record(id, values, ctx); context("could not parse TEST_HEALTHY")?;
            };
            if healthy {
                state.status = Status::StoppingUpdaterAfterSuccess;
            } else {
                state.status = Status::StoppingNew;
            }
            StepResult::Transition(state)
        }
        Status::StoppingNew => StepResult::ShutdownProcess,
        Status::StoppingUpdaterAfterSuccess => {
            if let Err(err) = stop_container_idempotent(docker, &state.updater_container_id).await {
                // TODO: ignore if missing?
                error!("failed to stop updater: {}; will retry", err);
                return StepResult::Retry;
            }
            if let Err(err) = rename_container_idempotent(
                docker,
                &state.updater_container_id,
                &format!(
                    "{}-{}-updater-succeeded",
                    &state.base_name, &state.time_suffix
                ),
            )
            .await
            {
                // TODO: ignore if keeps failing or missing?
                error!("failed to rename updater: {}; will retry", err);
                return StepResult::Retry;
            }
            if let Err(err) = rename_container_idempotent(
                docker,
                &state.old_container_id,
                &format!("{}-{}-old-replaced", &state.base_name, &state.time_suffix),
            )
            .await
            {
                // TODO: ignore if keeps failing or missing?
                error!("failed to rename old: {}; will retry", err);
                return StepResult::Retry;
            }
            state.status = Status::None;
            state.current_version = state.next_version;
            state.next_version = "".into();
            state.old_container_id = "".into();
            state.updater_container_id = "".into();
            state.new_container_id = "".into();
            state.base_name = "".into();
            state.time_suffix = "".into();
            StepResult::Transition(state)
        }
        Status::None => StepResult::StateMachineDone,
        _ => StepResult::Sleep,
    }
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

        // TODO: sanity check here also?
        // make sure our id is as expected, we have access to things, ...?

        run_update(
            &docker,
            &storage,
            update_from_updater_step,
            cancellation.clone(),
        )
        .await?;

        return Ok(());
    }

    if autoupdate_enabled {
        // do some sanity checks: only run if have access to docker socket, sqlite3, we know ourselves, and we are marked auto restart and not delete on exit
        // TODO: if we are not running a named tag, do an explicit update to the current sha256??? (or even refuse to start???)

        let docker = Docker::connect_with_local_defaults()?;
        let self_container = identify_self(&docker).await?;
        let storage = Storage::init()?;
        let (state, _version) = storage.get_state()?;

        // TODO: if status is PullingImage, allow program to start???

        info!("read state {:?}", state);

        if state.status != Status::None {
            //
            // some kind of update is running. figure out if we are the old or the new version and run appropriately?
            // TODO: i think we can safely run "the app" below even while the update is running (except if we migrate stuff and we haven't yet committed to upgrading by passing a healthcheck??? tricky)
            if &state.old_container_id == self_container.id.as_ref().unwrap() {
                run_update(
                    &docker,
                    &storage,
                    update_from_old_step,
                    cancellation.clone(),
                )
                .await?;
            } else if &state.new_container_id == self_container.id.as_ref().unwrap() {
                run_update(
                    &docker,
                    &storage,
                    update_from_new_step,
                    cancellation.clone(),
                )
                .await?;
            } else {
                bail!("confusing state");
                // complain. we are running with a selfupdate state we don't recognize so maybe it's shared or something? should be cleaned up before we run.
                // declare we do not care about this update? kind of janky... this happens if somehow the db is stale or shared?
            }
        }

        // ok... now we are running successfully. run a checker in the background:
        {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                loop {
                    if let Err(err) =
                        update_loop_body(&docker, &storage, cancellation.clone()).await
                    {
                        error!("update loop error: {}", err);
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn cleanup(docker: &Docker) {
        for name in vec!["selfupdater-unittest-a", "selfupdater-unittest-b"] {
            _ = docker
                .stop_container(
                    name,
                    Some(bollard::query_parameters::StopContainerOptions {
                        signal: Some("kill".into()),
                        t: None,
                    }),
                )
                .await;
            _ = docker
                .remove_container(
                    name,
                    Some(bollard::query_parameters::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    }

    #[tokio::test]
    #[test_log::test]
    async fn docker_idempotence() -> Result<()> {
        let docker = Docker::connect_with_local_defaults()?;
        cleanup(&docker).await;

        let test_image = "debian:bookworm-slim";

        if !check_if_image_exists(&docker, test_image).await? {
            info!("pulling test image");
            pull_image(&docker, test_image).await?;
        }

        info!("creating test container");
        let test_container = docker
            .create_container(
                Some(bollard::query_parameters::CreateContainerOptions {
                    name: Some("selfupdater-unittest-a".into()),
                    ..Default::default()
                }),
                bollard::models::ContainerCreateBody {
                    image: Some(test_image.into()),
                    ..Default::default()
                },
            )
            .await?;

        for _ in 0..10 {
            info!("starting first time");
            start_container_idempotent(&docker, &test_container.id).await?;
            info!("starting second time");
            start_container_idempotent(&docker, &test_container.id).await?;

            info!("stopping first time");
            stop_container_idempotent(&docker, &test_container.id).await?;
            info!("stopping second time");
            stop_container_idempotent(&docker, &test_container.id).await?;

            info!("renaming to b first time");
            rename_container_idempotent(&docker, &test_container.id, "selfupdater-unittest-b")
                .await?;
            info!("renaming to b second time");
            rename_container_idempotent(&docker, &test_container.id, "selfupdater-unittest-b")
                .await?;

            info!("renaming to a first time");
            rename_container_idempotent(&docker, &test_container.id, "selfupdater-unittest-a")
                .await?;
            info!("renaming to a first time");
            rename_container_idempotent(&docker, &test_container.id, "selfupdater-unittest-a")
                .await?;
        }

        // bail!("wtf");

        cleanup(&docker).await;
        Ok(())
    }
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
