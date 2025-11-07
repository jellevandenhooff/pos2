# dockerloader

A self-updating container loader for Rust applications. dockerloader enables your application to automatically update itself from an OCI registry with safe rollback on failures.

## Architecture

### Components

- **dockerloader** (supervisor): Main process that manages the application lifecycle
- **dockerloaded** (application): Example application that uses dockerloader for self-updates

### How It Works

1. **Supervisor Pattern**: dockerloader runs as the main process and spawns your application as a subprocess
2. **Update Detection**: Application checks for new versions in the OCI registry
3. **Safe Updates**: New versions are tested in "trial mode" before being committed
4. **Automatic Rollback**: If trial times out or fails, the previous version is restored
5. **File-based IPC**: Symlinks and exit codes are used for communication between supervisor and app

### IPC Contract

**Symlinks:**
- `/data/dockerloader/entrypoint` - Current stable version
- `/data/dockerloader/entrypoint-attempt` - Pending update (created by app)
- `/data/dockerloader/entrypoint-attempting` - Currently being tested (created by supervisor)

**Marker Files:**
- `/data/dockerloader/update-attempt` - Contains SHA of failed update attempt (prevents retry)

**Exit Codes:**
- Exit code **42** - Signals supervisor to restart (update available)
- Other exit codes - Supervisor exits with same code

## Storage

- `/data/dockerloader/storage/v1/blobs/` - OCI blob cache (layers, configs)
- `/data/dockerloader/storage/v1/images/` - Downloaded manifests
- `/data/dockerloader/storage/v1/extracted/` - Extracted image filesystems

Old images and unreferenced blobs are cleaned up when `mark_ready()` is called.

## Environment Variables

### Required

- `DOCKERLOADER_TARGET` - OCI image reference to monitor (e.g., `localhost:5050/myapp:latest`)

### Optional

- `DOCKERLOADER_TRIAL_TIMEOUT_MS` - Trial mode timeout in milliseconds (default: 10000)
- `DOCKERLOADER_TRIAL` - Set by supervisor during trial runs (do not set manually)

### Application-specific

Your application can include a `.env` file in the image root. Variables from this file are loaded during `init_dockerloaded()`.

## Usage in Your Application

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

## Background Update Loop (Coming Soon)

A background update loop function is being added to enable continuous update checking:

```rust
// This will be available soon
dockerloader::start_update_loop().await?;
```

Configuration:
- `DOCKERLOADER_UPDATE_INTERVAL_SECS` - How often to check for updates (default: 300 = 5 minutes)

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
- Storage cleanup
