# TODO

## wasi3experiment

- [ ] use tunnel client state to actually connect to tunnel
  - load tunnel config from config.json
  - if tunnel config exists, start tunnel client
  - proxy requests from tunnel to local listener

## tunnel

- [ ] make tunnel setup flow testable locally
  - tunnel setup already extracted to reusable functions
  - need to mock/skip github OAuth login for testing
  - need to mock/skip ACME certificate acquisition for testing
  - consider using test doubles or feature flags to enable local testing mode
- [ ] extract authentication and user management from tunnel
  - tunnel will become just a feature of a broader platform
  - OAuth2 flow, user management, and domain assignment should be separate service
- [ ] improve local testing infrastructure
  - local DNS resolution setup needs better documentation
  - local certificate trust configuration needs improvement
  - streamline development workflow
