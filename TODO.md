# TODO

## wasi3experiment

- [ ] use tunnel client state to actually connect to tunnel
  - load tunnel config from config.json
  - if tunnel config exists, start tunnel client
  - proxy requests from tunnel to local listener

## tunnel

- [ ] fix tunnel build errors
  - compilation failing due to missing features/dependencies in axum and other crates
  - caused by dependency reorganization
  - need to update Cargo.toml with correct feature flags

- [ ] make tunnel setup flow testable locally
  - tunnel setup already extracted to reusable functions
  - need to mock/skip github OAuth login for testing
  - need to mock/skip ACME certificate acquisition for testing
  - consider using test doubles or feature flags to enable local testing mode
