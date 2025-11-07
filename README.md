# pos2

## architecture

The system consists of two components:

- supervisor (`dockerloader`): manages the runtime lifecycle, downloads updates from OCI registries, and spawns the runtime as a subprocess
- runtime (`wasi3experiment`): the actual application that runs user workloads

The supervisor downloads the runtime image, extracts it to `/data/dockerloader`, and spawns `/data/dockerloader/entrypoint`. When updates are available, the supervisor can trial new versions with automatic rollback on failure.

## building

- supervisor: `Dockerfile.dockerloader`
- runtime: `Dockerfile.wasi3experiment`

## development

workspace dependencies are managed with `cargo-autoinherit`. run `cargo autoinherit` after modifying workspace dependencies to ensure consistency across crates

## CI

`.github/workflows/combined.yml` builds both supervisor and runtime for amd64 and arm64, then combines them into multiarch manifests:
- `ghcr.io/jellevandenhooff/pos2:supervisor-main`
- `ghcr.io/jellevandenhooff/pos2:runtime-main`

## installation

`scripts/install.sh` sets up pos2 by:
1. creating `$HOME/pos2data` directory
2. pulling the supervisor image
3. running the supervisor container with `DOCKERLOADER_TARGET` pointing to the runtime image

The supervisor then downloads and manages the runtime automatically.

## updating

The runtime checks for updates based on `DOCKERLOADER_UPDATE_INTERVAL_SECS`. New versions are trialed with a timeout - if the runtime doesn't commit the trial within the timeout period, the supervisor rolls back to the previous version.
