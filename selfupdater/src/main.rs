use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use bollard::{models::*, query_parameters::*};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

mod docker;
mod storage;

// TODO:
// - repo, cleaning times, checking times configurable
// - (briefly) write up the plan
// - end to end tests for multiple updates
// - hook up the health check
// - fun end to end test with injected failure before/after every step
// - logging options (for rotating logs?)
// - timeouts on docker API calls / external operations?
// - see if moving the state variables is nice?
// - adapt selfupdater as library

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
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

struct UpdaterConfiguration {
    repository: String,
    tag: String,
    delete_dangling_created_containers_after: chrono::Duration,
    delete_unused_containers_after: chrono::Duration,
    delete_unused_images_pulled_before: chrono::Duration, // XXX: yuck
}

fn diff_states(a: &State, b: &State) -> Result<String> {
    let a = serde_json::to_value(a)?;
    let a = a.as_object().context("bad json")?;
    let b = serde_json::to_value(b)?;
    let b = b.as_object().context("bad json")?;
    let mut changes = vec![];
    for key in a.keys() {
        if a[key] != b[key] {
            changes.push(format!("{}: {} -> {}", key, a[key], b[key]));
        }
    }
    Ok(changes.join(", "))
}

// flow: create container, store id, start container

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
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

enum Action {
    ShutdownProcess,
    Transition(State),
    Sleep,
    Retry,
    StateMachineDone,
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

struct SelfUpdater {
    docker: docker::Client,
    storage: storage::Storage,
    config: UpdaterConfiguration,
}

impl SelfUpdater {
    async fn new(config: UpdaterConfiguration) -> Result<SelfUpdater> {
        Ok(Self {
            docker: docker::Client::new().await?,
            storage: storage::Storage::init()?,
            config: config,
        })
    }

    async fn clean_up_images_and_containers(&self) -> Result<()> {
        // TODO: make these times configurable
        let now = chrono::Utc::now();
        let cutoff_old_created = now - self.config.delete_dangling_created_containers_after;
        let cutoff_old_exited = now - self.config.delete_unused_containers_after;
        let cutoff_old_pulled = now - self.config.delete_unused_images_pulled_before;

        let containers = self
            .docker
            .client
            .list_containers(Some(ListContainersOptions {
                all: true,
                ..Default::default()
            }))
            .await?;

        let mut in_usage_image_ids = HashSet::new();

        let tracked_container_ids = HashSet::<String>::from_iter(
            self.storage
                .list_tracked_resources("container")?
                .into_iter(),
        );

        let mut seen_container_ids = HashSet::<String>::new();

        for container in containers {
            let container_id = match &container.id {
                Some(id) => id,
                None => continue,
            };

            // Remove old container that are stuck in the CREATED state. We
            // might create containersr and not successfully record their ids
            // so to prevent leaking containers we delete all stuck containers,
            // even if we are not sure if we created them.
            if container.state == Some(ContainerSummaryStateEnum::CREATED) {
                if let Some(created) = container
                    .created
                    .and_then(|timestamp| chrono::DateTime::from_timestamp_secs(timestamp))
                    && created < cutoff_old_created
                {
                    info!(
                        "found old container {:?} with names {:?} still in created state; assuming it is unused and deleting it",
                        container.id, container.names
                    );

                    self.docker.remove_container(container_id).await?;
                }
            }

            // Remove old exited containers we know about.
            if tracked_container_ids.contains(container_id)
                && container.state == Some(ContainerSummaryStateEnum::EXITED)
            {
                let inspected = self
                    .docker
                    .client
                    .inspect_container(
                        container.id.as_ref().unwrap(),
                        Some(InspectContainerOptions {
                            ..Default::default()
                        }),
                    )
                    .await?;

                if let Some(time) = inspected
                    .state
                    .and_then(|state| state.finished_at)
                    .and_then(|time_str| chrono::DateTime::parse_from_rfc3339(&time_str).ok())
                    && time.to_utc() < cutoff_old_exited
                {
                    info!(
                        "deleting unused and exited container {:?} with names {:?}",
                        container.id, container.names
                    );
                    self.docker.remove_container(container_id).await?;
                    // don't mark this container or its images as in-use
                    continue;
                }
            }

            if let Some(id) = container.image_id {
                in_usage_image_ids.insert(id);
            }
            seen_container_ids.insert(container_id.clone());
        }

        for container_id in tracked_container_ids {
            if !seen_container_ids.contains(&container_id) {
                info!(
                    "did not see tracked container {}, removing it from our list of tracked containers",
                    container_id
                );
                self.storage
                    .remove_tracked_resource("container", &container_id)?;
            }
        }

        let images = self
            .docker
            .client
            .list_images(Some(ListImagesOptions {
                all: true,
                ..Default::default()
            }))
            .await?;

        let tracked_image_ids =
            HashSet::<String>::from_iter(self.storage.list_tracked_resources("image")?.into_iter());

        let mut seen_image_ids = HashSet::<String>::new();

        for image in images {
            if tracked_image_ids.contains(&image.id) && !in_usage_image_ids.contains(&image.id) {
                let inspected = self.docker.client.inspect_image(&image.id).await?;

                // Here we track images by id and not by tag. That is a little bit too bad...

                if let Some(time) = inspected
                    .metadata
                    .and_then(|metadata| metadata.last_tag_time)
                    .and_then(|time_str| chrono::DateTime::parse_from_rfc3339(&time_str).ok())
                    && time.to_utc() < cutoff_old_pulled
                {
                    info!(
                        "found unused image {:?} with digests {:?} and tags {:?}; deleting it",
                        inspected.id, inspected.repo_digests, inspected.repo_tags,
                    );
                    self.docker.remove_image(&image.id).await?;
                    continue;
                }
            }

            seen_image_ids.insert(image.id);
        }

        for image_id in tracked_image_ids {
            if !seen_image_ids.contains(&image_id) {
                info!(
                    "did not see tracked image {}, removing it from our list of tracked images",
                    image_id
                );
                self.storage.remove_tracked_resource("image", &image_id)?;
            }
        }

        Ok(())
    }
    async fn create_updater_container(&self, state: &State) -> Result<String> {
        let current_container = self
            .docker
            .client
            .inspect_container(
                &state.old_container_id,
                Some(InspectContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;

        let options = CreateContainerOptions {
            name: Some(format!("{}-{}-updater", state.base_name, state.time_suffix)),
            ..Default::default()
        };

        let body = ContainerCreateBody {
            image: Some(state.next_version.clone()),
            cmd: Some(vec!["selfupdater-helper".into()]), // must exist...
            host_config: Some(HostConfig {
                binds: current_container.host_config.unwrap().binds,
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::NO),
                    maximum_retry_count: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .client
            .create_container(Some(options), body)
            .await?;
        Ok(created.id)
    }

    async fn create_new_container(&self, state: &State) -> Result<String> {
        let current_container = self
            .docker
            .client
            .inspect_container(
                &state.old_container_id,
                Some(InspectContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;

        let options = CreateContainerOptions {
            name: Some(format!(
                "{}-{}-new-pending",
                state.base_name, state.time_suffix
            )),
            ..Default::default()
        };

        let body = ContainerCreateBody {
            image: Some(state.next_version.clone()),
            // cmd: Some(vec!["/selfupdater".into()]), // TODO: copy from old?
            host_config: Some(HostConfig {
                binds: current_container.host_config.unwrap().binds,
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::NO),
                    maximum_retry_count: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .client
            .create_container(Some(options), body)
            .await?;
        Ok(created.id)
    }

    async fn consider_update(
        &self,
        next_version: String,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let (mut state, version) = self.storage.get_state()?;
        // TODO: if current version is none, initialize to current container version?
        // TODO: set old container id to self if empty? if not empty, complain? (maybe allow resetting if it mentions a non-existent container?)

        if state.status == Status::None
            && next_version != state.current_version
            && next_version != state.next_version
        {
            let self_container = self.docker.identify_self().await?;
            state.old_container_id = self_container.container_id;
            state.new_container_id = "".into(); // TODO set these to empty when transitioning to NONE state?
            state.updater_container_id = "".into();

            state.time_suffix = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
            state.base_name = self_container.name;
            state.status = Status::PullingImage;
            state.next_version = next_version;
            state.last_change_at = Some(chrono::Utc::now());
            self.storage.update_state(&state, version)?;
            self.run_update(Self::update_from_old_step, cancellation)
                .await?;
        }

        // TODO: maybe stop doing work in the main process once we enter run_update_from_old (?)

        Ok(())
    }

    async fn update_loop_body(&self, cancellation: CancellationToken) -> Result<()> {
        let next_version = self
            .docker
            .check_for_new_image(&self.config.repository, &self.config.tag)
            .await?;
        self.consider_update(next_version, cancellation).await?;
        self.clean_up_images_and_containers().await?;
        Ok(())
    }

    async fn run_update(
        &self,
        step_func: impl AsyncFn(&Self, State) -> Action,
        cancellation_token: CancellationToken,
    ) -> Result<()> {
        let mut prev_state = None;

        loop {
            let (state, old_version) = self.storage.get_state()?;
            if let Some(prev_state) = prev_state
                && prev_state != state
            {
                info!(
                    "state changed by another process: {}",
                    diff_states(&prev_state, &state)?
                );
            }
            prev_state = Some(state.clone());
            match step_func(self, state).await {
                Action::ShutdownProcess => {
                    // we wait for the cancellation token to hit so we do not
                    // immediately restart because of docker's restart policy
                    cancellation_token.cancelled().await;
                    std::process::exit(0);
                }
                Action::Transition(mut new_state) => {
                    new_state.last_change_at = Some(chrono::Utc::now());
                    // TODO: in a test, just randomly restart here?
                    match self.storage.update_state(&new_state, old_version) {
                        Ok(_) => {
                            prev_state = Some(new_state);
                            // TODO: in a test, just randomly restart here?
                            continue;
                        }
                        Err(err) => {
                            error!("failed to do update state: {}; will wait and retry", err);
                            // TODO: exponential back-off? bound number of failures?
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        }
                    }
                }
                Action::Retry => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                Action::Sleep => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                Action::StateMachineDone => return Ok(()),
            }
        }
    }

    async fn update_from_old_step(&self, mut state: State) -> Action {
        match state.status {
            Status::PullingImage => {
                info!("pulling image {}", &state.next_version);
                match self.docker.pull_image(&state.next_version).await {
                    Ok(id) => {
                        info!(
                            "successfully pulled image {} with id {}",
                            &state.next_version, &id
                        );
                        if let Err(err) = self.storage.add_tracked_resources("image", &id) {
                            error!(
                                "failed to image {} to tracked resources; ignoring error: {}",
                                &id, &err
                            )
                        }
                        state.status = Status::CreatingUpdater;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: retry a couple of times?
                        error!("failed to pull image: {}; will rollback", err);
                        state.status = Status::StoppingUpdaterAfterFailure;
                        Action::Transition(state)
                    }
                }
            }
            Status::CreatingUpdater => {
                info!("creating updater container");
                match self.create_updater_container(&state).await {
                    Ok(id) => {
                        info!("successfully created updater container {}", id);
                        if let Err(err) = self.storage.add_tracked_resources("container", &id) {
                            error!(
                                "failed to add container {} to tracked resources; ignoring error: {}",
                                &id, &err
                            )
                        }
                        state.updater_container_id = id;
                        state.status = Status::StartingUpdater;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: retry a couple of times?
                        error!("failed to create updater: {}; will rollback", err);
                        state.status = Status::StoppingUpdaterAfterFailure;
                        Action::Transition(state)
                    }
                }
            }
            Status::StartingUpdater => {
                info!("starting updater container {}", state.updater_container_id);
                match self
                    .docker
                    .start_container(&state.updater_container_id)
                    .await
                {
                    Ok(_) => {
                        info!(
                            "successfully started updater container {}",
                            state.updater_container_id
                        );
                        state.status = Status::StartedUpdater;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: retry a couple of times?
                        error!("failed to start updater: {}", err);
                        state.status = Status::StoppingUpdaterAfterFailure;
                        Action::Transition(state)
                    }
                }
            }
            Status::StartedUpdater => {
                // TODO: more elegant timeout somehow?
                if state.last_change_at.is_some_and(|last_change| {
                    chrono::Utc::now() >= last_change + chrono::Duration::seconds(10)
                }) {
                    info!(
                        "updater container {} stayed in started state for too long without marking itself healthy",
                        state.new_container_id
                    );
                    state.status = Status::StoppingUpdaterAfterFailure;
                    Action::Transition(state)
                } else {
                    info!(
                        "waiting for updater container {} to mark itself healthy",
                        state.updater_container_id
                    );
                    Action::Sleep
                }
            }
            Status::StoppingOld => Action::ShutdownProcess,
            Status::StoppingUpdaterAfterFailure => {
                if state.updater_container_id != "" {
                    info!(
                        "stopping and renaming updater container {}",
                        state.updater_container_id
                    );
                    if let Err(err) = self
                        .docker
                        .stop_and_rename_container(
                            &state.updater_container_id,
                            &format!("{}-{}-updater-failed", &state.base_name, &state.time_suffix),
                        )
                        .await
                    {
                        // TODO: ignore if missing?
                        error!("failed to stop and rename updater: {}; will retry", err);
                        return Action::Retry;
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
                Action::Transition(state)
            }
            Status::None => Action::StateMachineDone,
            _ => Action::Sleep,
        }
    }

    async fn update_from_updater_step(&self, mut state: State) -> Action {
        match state.status {
            Status::StartedUpdater => {
                state.status = Status::StoppingOld;
                Action::Transition(state)
                // on failure, we will be stopped by old
            }
            Status::StoppingOld => {
                info!(
                    "stopping and renaming old container {}",
                    &state.old_container_id
                );
                match self
                    .docker
                    .stop_and_rename_container(
                        &state.old_container_id,
                        &format!("{}-{}-old", &state.base_name, &state.time_suffix),
                    )
                    .await
                {
                    Ok(_) => {
                        info!("successfully stopped and renamed old");
                        state.status = Status::CreatingNew;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: retry a couple of times?
                        error!("failed to stop old: {}; will rollback", err);
                        state.status = Status::RestartingOld;
                        Action::Transition(state)
                    }
                }
            }
            Status::CreatingNew => {
                info!("creating new container",);
                match self.create_new_container(&state).await {
                    Ok(id) => {
                        info!("successfully created new container {}", id);
                        if let Err(err) = self.storage.add_tracked_resources("container", &id) {
                            error!(
                                "failed to add container {} to tracked resources; ignoring error: {}",
                                &id, &err
                            )
                        }
                        state.new_container_id = id;
                        state.status = Status::StartingNew;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: retry a couple of times?
                        error!("failed to create new: {}; will rollback", err);
                        state.status = Status::RestartingOld;
                        Action::Transition(state)
                    }
                }
            }
            Status::StartingNew => {
                info!(
                    "renaming and starting new container {}",
                    state.new_container_id
                );
                match self
                    .docker
                    .rename_and_start_container(&state.new_container_id, &state.base_name)
                    .await
                {
                    Ok(_) => {
                        info!("successfully renamed and started new container");
                        state.status = Status::StartedNew;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        error!("failed to rename and start new: {}; will rollback", err);
                        state.status = Status::StoppingNew;
                        Action::Transition(state)
                    }
                }
            }
            Status::StartedNew => {
                if state.last_change_at.is_some_and(|last_change| {
                    chrono::Utc::now() >= last_change + chrono::Duration::seconds(10)
                }) {
                    info!(
                        "new container {} stayed in started state for too long without marking itself healthy",
                        state.new_container_id
                    );
                    state.status = Status::StoppingNew;
                    Action::Transition(state)
                } else {
                    info!(
                        "waiting for new container {} to mark itself healthy",
                        state.new_container_id
                    );
                    Action::Sleep
                }
                // this is the failure handler
            }
            Status::StoppingNew => {
                info!(
                    "stopping and renaming new container {}",
                    state.new_container_id
                );
                match self
                    .docker
                    .stop_and_rename_container(
                        &state.new_container_id,
                        &format!("{}-{}-new-failed", &state.base_name, &state.time_suffix),
                    )
                    .await
                {
                    Ok(_) => {
                        info!("successfully stopped and renamed new");
                        state.status = Status::RestartingOld;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        // TODO: ignore if missing?
                        error!("failed to stop and rename new: {}; will retry", err);
                        Action::Retry
                    }
                }
            }
            Status::RestartingOld => {
                info!(
                    "renaming and starting old container {}",
                    state.old_container_id
                );
                match self
                    .docker
                    .rename_and_start_container(&state.old_container_id, &state.base_name)
                    .await
                {
                    Ok(_) => {
                        info!("successfully renamed and started old container");
                        state.status = Status::StoppingUpdaterAfterFailure;
                        Action::Transition(state)
                    }
                    Err(err) => {
                        error!("failed to rename and start old: {}; will retry", err);
                        Action::Retry
                    }
                }
                // cannot fail (hopefully)
            }
            Status::StoppingUpdaterAfterSuccess | Status::StoppingUpdaterAfterFailure => {
                Action::ShutdownProcess
            }
            _ => Action::Sleep,
        }
    }

    async fn update_from_new_step(&self, mut state: State) -> Action {
        match state.status {
            Status::StartedNew => {
                info!("started new; doing a healthcheck",);
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
                Action::Transition(state)
            }
            Status::StoppingNew => Action::ShutdownProcess,
            Status::StoppingUpdaterAfterSuccess => {
                info!(
                    "stopping and renaming updater container {}",
                    state.updater_container_id
                );
                if let Err(err) = self
                    .docker
                    .stop_and_rename_container(
                        &state.updater_container_id,
                        &format!(
                            "{}-{}-updater-succeeded",
                            &state.base_name, &state.time_suffix
                        ),
                    )
                    .await
                {
                    // TODO: ignore if missing?
                    error!("failed to stop updater: {}; will retry", err);
                    return Action::Retry;
                }
                state.status = Status::None;
                state.current_version = state.next_version;
                state.next_version = "".into();
                state.old_container_id = "".into();
                state.updater_container_id = "".into();
                state.new_container_id = "".into();
                state.base_name = "".into();
                state.time_suffix = "".into();
                Action::Transition(state)
            }
            Status::None => Action::StateMachineDone,
            _ => Action::Sleep,
        }
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
            let signal = tokio::select! {
                _ = interrupt.recv() => {
                    "interrupt"
                }
                _ = terminate.recv() => {
                    "terminate"
                }
            };
            info!("received {} signal, stopping...", signal);
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

    let updater_config = UpdaterConfiguration {
        repository: "localhost:5050/selfupdater".into(),
        tag: "testing".into(),
        delete_dangling_created_containers_after: chrono::Duration::seconds(30),
        delete_unused_containers_after: chrono::Duration::seconds(30),
        delete_unused_images_pulled_before: chrono::Duration::seconds(30),
    };

    if args.get(1).is_some_and(|cmd| cmd == "selfupdater-helper") {
        let updater = SelfUpdater::new(updater_config).await?;

        // TODO: sanity check here also?
        // make sure our id is as expected, we have access to things, ...?

        updater
            .run_update(SelfUpdater::update_from_updater_step, cancellation.clone())
            .await?;

        return Ok(());
    }

    if autoupdate_enabled {
        let updater = SelfUpdater::new(updater_config).await?;

        // do some sanity checks: only run if have access to docker socket, sqlite3, we know ourselves, and we are marked auto restart and not delete on exit

        let self_container = updater.docker.identify_self().await?;
        let (state, _version) = updater.storage.get_state()?;

        // TODO: if status is PullingImage, allow program to start???

        info!("read state {:?}", state);

        // TODO: only add current container to tracked resources on initial state creation?????
        // TODO: on initial state creation, if we pull the latest image and its version matches ours, don't update
        // but instead record that version?
        // TODO: think about container configuration that we do or do not want to copy. detect defaults vs
        // customization? complain if there are things we do not want to handle?
        updater
            .storage
            .add_tracked_resources("container", &self_container.container_id)?;
        updater
            .storage
            .add_tracked_resources("image", &self_container.image_id)?;

        if state.status != Status::None {
            // some kind of update is running. figure out if we are the old or the new version and run appropriately?
            // TODO: i think we can safely run "the app" below even while the update is running (except if we migrate stuff and we haven't yet committed to upgrading by passing a healthcheck??? tricky)
            if state.old_container_id == self_container.container_id {
                updater
                    .run_update(SelfUpdater::update_from_old_step, cancellation.clone())
                    .await?;
            } else if state.new_container_id == self_container.container_id {
                updater
                    .run_update(SelfUpdater::update_from_new_step, cancellation.clone())
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
                    if let Err(err) = updater.update_loop_body(cancellation.clone()).await {
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

    async fn cleanup(docker: &docker::Client) {
        for name in vec!["selfupdater-unittest-a", "selfupdater-unittest-b"] {
            _ = docker
                .client
                .stop_container(
                    name,
                    Some(StopContainerOptions {
                        signal: Some("kill".into()),
                        t: None,
                    }),
                )
                .await;
            _ = docker
                .client
                .remove_container(
                    name,
                    Some(RemoveContainerOptions {
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
        // let updater = SelfUpdater::new().await?;
        let docker = docker::Client::new().await?;

        cleanup(&docker).await;

        let test_image = "debian:bookworm-slim";

        if !docker.check_if_image_exists(test_image).await? {
            info!("pulling test image");
            docker.pull_image(test_image).await?;
        }

        info!("creating test container");
        let test_container = docker
            .client
            .create_container(
                Some(CreateContainerOptions {
                    name: Some("selfupdater-unittest-a".into()),
                    ..Default::default()
                }),
                ContainerCreateBody {
                    image: Some(test_image.into()),
                    ..Default::default()
                },
            )
            .await?;

        for _ in 0..10 {
            info!("starting first time");
            docker.start_container(&test_container.id).await?;
            info!("starting second time");
            docker.start_container(&test_container.id).await?;

            info!("stopping first time");
            docker.stop_container(&test_container.id).await?;
            info!("stopping second time");
            docker.stop_container(&test_container.id).await?;

            info!("renaming to b first time");
            docker
                .rename_container(&test_container.id, "selfupdater-unittest-b")
                .await?;
            info!("renaming to b second time");
            docker
                .rename_container(&test_container.id, "selfupdater-unittest-b")
                .await?;

            info!("renaming to a first time");
            docker
                .rename_container(&test_container.id, "selfupdater-unittest-a")
                .await?;
            info!("renaming to a first time");
            docker
                .rename_container(&test_container.id, "selfupdater-unittest-a")
                .await?;
        }

        // bail!("wtf");

        cleanup(&docker).await;
        Ok(())
    }
}
