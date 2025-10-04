use anyhow::{Context, Result, anyhow, bail};
use bollard::{Docker, models::*, query_parameters::*};
use futures::stream::StreamExt;
use itertools::Itertools;
use tracing::{error, info};

pub struct Client {
    pub client: Docker,
}

pub struct SelfInfo {
    pub container_id: String,
    pub image_id: String,
    pub name: String,
}

impl Client {
    pub async fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self { client: docker })
    }

    pub async fn check_for_new_image(&self, repo: &str, channel: &str) -> Result<String> {
        let reference_string = format!("{}:{}", repo, channel);

        // TODO: this call is pretty slow... why?
        let image = self
            .client
            .inspect_registry_image(&reference_string, None)
            .await?;

        let hash = image.descriptor.digest.context("missing digest")?;
        Ok(format!("{}@{}", repo, hash))
    }

    pub async fn check_if_image_exists(&self, reference: &str) -> Result<bool> {
        match self.client.inspect_image(reference).await {
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

    pub async fn pull_image(&self, image: &str) -> Result<String> {
        let options = Some(CreateImageOptions {
            from_image: Some(image.into()), // can include tag
            ..Default::default()
        });
        let credentials = None;
        let mut output = self.client.create_image(options, None, credentials.clone());
        while let Some(line) = output.next().await {
            match line {
                Ok(update) => info!("pulling: {:?}", update),
                Err(err) => {
                    error!("error pulling image: {err}");
                    bail!("error pulling image: {err}");
                }
            }
        }
        let inspected = self.client.inspect_image(image).await?;
        let id = inspected.id.context("missing id")?;
        Ok(id)
    }

    pub async fn rename_container(&self, container_id: &str, name: &str) -> Result<()> {
        let current = self
            .client
            .inspect_container(
                &container_id,
                Some(InspectContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;
        if current.name.unwrap().trim_start_matches("/") == name {
            return Ok(());
        }
        self.client
            .rename_container(container_id, RenameContainerOptions { name: name.into() })
            .await?;
        Ok(())
    }

    pub async fn stop_container(&self, container_id: &str) -> Result<()> {
        // TODO: think about missing containers?
        self.client
            .update_container(
                container_id,
                ContainerUpdateBody {
                    restart_policy: Some(RestartPolicy {
                        name: Some(RestartPolicyNameEnum::NO),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        self.client
            .stop_container(
                container_id,
                Some(StopContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_container(&self, container_id: &str) -> Result<()> {
        self.client
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_image(&self, image_id: &str) -> Result<()> {
        self.client
            .remove_image(
                &image_id,
                Some(RemoveImageOptions {
                    ..Default::default()
                }),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn stop_and_rename_container(&self, container_id: &str, name: &str) -> Result<()> {
        self.stop_container(container_id).await?;
        self.rename_container(container_id, name).await?;
        Ok(())
    }

    pub async fn start_container(&self, container_id: &str) -> Result<()> {
        self.client
            .start_container(
                container_id,
                Some(StartContainerOptions {
                    ..Default::default()
                }),
            )
            .await?;
        self.client
            .update_container(
                container_id,
                ContainerUpdateBody {
                    restart_policy: Some(RestartPolicy {
                        name: Some(RestartPolicyNameEnum::ALWAYS),
                        maximum_retry_count: None,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    pub async fn rename_and_start_container(&self, container_id: &str, name: &str) -> Result<()> {
        self.rename_container(container_id, name).await?;
        self.start_container(container_id).await?;
        Ok(())
    }

    pub async fn identify_self(&self) -> Result<SelfInfo> {
        let options = Some(ListContainersOptions {
            all: true,
            filters: None, // Some(filters),
            ..Default::default()
        });

        let containers = self.client.list_containers(options).await?;

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

        let container_id = container.id.context("missing container id")?;
        let image_id = container.image_id.context("missing image id")?;
        let mut name = container
            .names
            .and_then(|names| names.into_iter().next())
            .context("missing name")?;
        if name.starts_with("/") {
            name.remove(0);
        }

        Ok(SelfInfo {
            container_id,
            image_id,
            name,
        })
    }
}
