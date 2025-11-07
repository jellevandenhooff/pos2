# dockerloader

A self-updating container loader for Rust applications. dockerloader enables your application to automatically update itself from an OCI registry with safe rollback on failures.

## Architecture

### Components

- dockerloader (supervisor): main process that manages the application lifecycle
- dockerloader-testapp (application): example application that uses dockerloader for self-updates

### How it works

1. supervisor pattern: dockerloader runs as the main process and spawns your application as a subprocess
2. update detection: application checks for new versions in the OCI registry
3. safe updates: new versions are tested in "trial mode" before being committed
4. automatic rollback: if trial times out or fails, the previous version is restored
5. file-based IPC: symlinks and exit codes are used for communication between supervisor and app

### IPC contract

Symlinks:
- `/data/dockerloader/entrypoint` - current stable version
- `/data/dockerloader/entrypoint-attempt` - pending update (created by app)
- `/data/dockerloader/entrypoint-attempting` - currently being tested (created by supervisor)

Marker files:
- `/data/dockerloader/update-attempt` - contains SHA of failed update attempt (prevents retry)

Exit codes:
- exit code 42 - signals supervisor to restart (update available)
- other exit codes - supervisor exits with same code

## Storage

- `/data/dockerloader/storage/v1/blobs/` - OCI blob cache (layers, configs)
- `/data/dockerloader/storage/v1/images/` - downloaded manifests
- `/data/dockerloader/storage/v1/extracted/` - extracted image filesystems

Old images and unreferenced blobs are cleaned up when `mark_ready()` is called.

## Environment variables

### Required

- `DOCKERLOADER_TARGET` - OCI image reference to monitor (e.g., `localhost:5050/myapp:latest`)

### Optional

- `DOCKERLOADER_TRIAL_TIMEOUT_MS` - trial mode timeout in milliseconds (default: 10000)
- `DOCKERLOADER_TRIAL` - set by supervisor during trial runs (do not set manually)

### Application-specific

Your application can include a `.env` file in the image root. Variables from this file are loaded during `init_dockerloaded()`.

## Usage in your application

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize dockerloader (loads .env from image)
    dockerloader::init_dockerloaded().await?;

    // Your application initialization here
    // ...

    // Mark ready - commits trial, cleans up storage, checks for updates
    dockerloader::mark_ready().await?;

    // Your application continues running
    // ...

    Ok(())
}
```

## Background update loop

Start a background update loop to continuously check for updates:

```rust
// Start background update checking
let _update_handle = dockerloader::start_update_loop().await?;
```

Configuration:
- `DOCKERLOADER_UPDATE_INTERVAL_SECS` - how often to check for updates (default: 300 = 5 minutes)

## CLI access

You can execute commands in the running application using `docker exec`:

```bash
docker exec <container-name> cli [args]
```

The supervisor will forward the command to the current stable entrypoint with `cli` as the first argument. Your application should check for this and handle CLI invocations appropriately (typically by skipping background tasks and update loops).

Example:
```rust
if dockerloader::is_cli_mode() {
    // Handle CLI command and exit
    return Ok(());
}
```

If the entrypoint hasn't been downloaded yet, the CLI command will wait for the initial download to complete.

## Testing

Run the integration tests:

```bash
cargo test -p dockerloader --test dockerloader_test -- --no-capture
```

The tests build test images, push them to a local registry, and verify:
- Complete update workflow (v1 → v2 → v3)
- Trial mode and commit logic
- Failed update handling and rollback
- Timeout handling
- Background update loop detection
- Storage cleanup
- CLI passthrough functionality

Test data location: Integration tests use `dockerloader/data/` for their data volume to avoid conflicts with application data in workspace `data/`.
