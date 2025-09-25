use std::{collections::HashMap, env};

use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use itertools::Itertools;

#[tokio::main]
async fn main() -> Result<()> {
    let docker = Docker::connect_with_local_defaults()?;

    let args: Vec<_> = std::env::args().collect();
    if let Some(command) = args.get(1)
        && command == "updater"
    {
        println!("running updater, bye!");
        return Ok(());
    }

    println!("Hello, world!");

    let mut filters = HashMap::new();
    // filters.insert("health", vec!["unhealthy"]);
    //
    let options = Some(bollard::query_parameters::ListContainersOptions {
        all: true,
        filters: Some(filters),
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

    docker.inspect_container(container_name, options)

    let image_id = container.image_id.unwrap();

    let create_container_options = bollard::query_parameters::CreateContainerOptions {
        // TODO: use name from current container?
        ..Default::default()
    };

    let create_container_body = bollard::models::ContainerCreateBody {
        image: Some(image_id),
        cmd: Some(vec!["/selfupdater".into(), "updater".into()]),
        host_config: Some(bollard::models::HostConfig {
            binds: Some(vec!["/var/run/docker.sock:/var/run/docker.sock".into()]),
            auto_remove: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };

    let response = docker
        .create_container(Some(create_container_options), create_container_body)
        .await?;

    docker
        .start_container(
            &response.id,
            Some(bollard::query_parameters::StartContainerOptions {
                ..Default::default()
            }),
        )
        .await?;

    for (key, value) in std::env::vars() {
        println!("{key}: {value}");
    }

    Ok(())
}
