# TODO

## wasi3experiment

- [ ] use tunnel client state to actually connect to tunnel
  - load tunnel config from config.json
  - if tunnel config exists, start tunnel client
  - proxy requests from tunnel to local listener

## tunnel

- [ ] add test mode for OAuth flow with integration test
  - add test_mode flag to ServerConfig and ServerState
  - bypass GitHub OAuth in test mode (simple login endpoint)
  - skip certificate fetching in test mode
  - create integration test similar to wasi3experiment that exercises full device flow
  - test should start server, run client setup, authenticate, approve device code, complete setup
- [ ] use docker container for cert/DNS testing
  - modify OS-level root cert store in container for Pebble ACME CA
  - configure DNS resolution at OS level in container
  - cleaner than patching multiple rustls dependencies
- [ ] extract authentication and user management from tunnel
  - tunnel will become just a feature of a broader platform
  - OAuth2 flow, user management, and domain assignment should be separate service
