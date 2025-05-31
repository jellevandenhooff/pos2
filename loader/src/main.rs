use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde::{self};
use serde_json;
use std::fs::File;
use std::io;
use std::path::Path;
use std::{env, fs};
use tar::Archive;
use tempdir::TempDir;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ConfigBlock {
    cmd: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ConfigFile {
    config: ConfigBlock,
}

async fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    println!("copying {:?} from {:?}", dst.as_ref(), src.as_ref());
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        // TODO: async? whatever
        let entry = entry?;
        println!(
            "entry name {:?} type {:?} is_dir {:?}",
            entry.file_name(),
            entry.file_type()?,
            entry.file_type()?.is_dir()
        );
        let ty = entry.file_type()?;
        if ty.is_dir() {
            Box::pin(copy_dir_all(
                entry.path(),
                dst.as_ref().join(entry.file_name()),
            ))
            .await?;
        } else if ty.is_symlink() {
            let link_target = tokio::fs::read_link(entry.path()).await?;
            std::os::unix::fs::symlink(link_target, dst.as_ref().join(entry.file_name()))
                .context("symlinking")?;
        } else {
            tokio::fs::copy(entry.path(), dst.as_ref().join(entry.file_name())).await?;
        }
    }
    Ok(())
}

fn inside_container() -> Result<()> {
    let val = std::env::var("container").context("getting env var")?;
    if val == "" {
        bail!("empty value");
    }
    Ok(())
}

async fn main2() -> Result<()> {
    let client = oci_distribution::Client::new(oci_distribution::client::ClientConfig::default());

    // reference -> digest -> config + layers
    // track current/pending/(others??)
    // link to /nix/store
    // download (in background?)
    // run (and restart?)

    match tokio::fs::try_exists("/data").await {
        Ok(true) => {}
        _ => {
            bail!("/data does not exist");
        }
    }

    let images_dir = Path::new("/data/images");

    tokio::fs::create_dir_all(images_dir).await?;

    let reference: oci_distribution::Reference = "ghcr.io/jellevandenhooff/innerapp:experiment"
        .parse()
        .unwrap();

    let (manifest, digest) = client
        .pull_manifest(
            &reference,
            &oci_distribution::secrets::RegistryAuth::Anonymous,
        )
        .await
        .context("pulling manifest")?;

    let ref_dir = images_dir.join(digest);

    if let Ok(true) = tokio::fs::try_exists(&ref_dir).await {
    } else {
        if let oci_distribution::manifest::OciManifest::Image(image) = &manifest {
            let mut out: Vec<u8> = Vec::new();
            let _result = client
                .pull_blob(&reference, &image.config, &mut out)
                .await
                .context("pulling config")?;

            tokio::fs::create_dir_all(&ref_dir).await?;
            std::fs::write(&ref_dir.join("config.json"), &out)?;

            let config: ConfigFile = serde_json::from_slice(&out)?;
            println!("path: {:?}", config.config.cmd);

            if image.layers.len() != 1 {
                bail!("expected exactly 1 layer");
            }

            let layer = &image.layers[0];

            {
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open("/tmp/test.tar.gz")
                    .await
                    .context("creating file")?;

                let _result = client.pull_blob(&reference, &layer, &mut file).await?;
            }

            let tar_gz = File::open("/tmp/test.tar.gz").context("read tarball")?;
            let tar = GzDecoder::new(tar_gz);
            let mut archive = Archive::new(tar);
            let tmp_dir = TempDir::new("example").context("creating temp dir")?;
            // fs::create_dir_all(&ref_dir)?;
            // archive.unpack(&ref_dir)?;
            archive.unpack(&tmp_dir)?;
            copy_dir_all(&tmp_dir, &ref_dir.join("extracted")).await?;
        } else {
            bail!("got index, expected single manifest");
        }
    }

    let bytes = std::fs::read(&ref_dir.join("config.json"))?;

    let config: ConfigFile = serde_json::from_slice(&bytes)?;
    println!("path: {:?}", config.config.cmd);

    // TODO: somehow check we are running inside docker, otherwise do not write here?

    let store_root = Path::new("/nix/store");

    for entry in std::fs::read_dir(&ref_dir.join("extracted/nix/store"))
        .context("reading extracted layer")?
    {
        let entry = entry?;
        println!("{:?}", entry.path());

        let target = store_root.join(entry.file_name());
        if let Ok(true) = std::fs::exists(&target) {
        } else {
            std::os::unix::fs::symlink(entry.path(), &target).context("symlinking")?;
        }
    }

    std::process::Command::new(&config.config.cmd[0])
        .status()
        .context("running")?;

    // println!("layer: {:?}", layer.digest);
    // image.config;

    // println!("manifest: {:?}", manifest);

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("checking to see if inside container");
    inside_container()
        .context("not running inside a container (based on container environment variable)")?;

    std::fs::create_dir_all(env::temp_dir())?;
    main2().await?;
    Ok(())
}
