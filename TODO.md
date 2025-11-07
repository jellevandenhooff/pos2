# TODO

## wasi3experiment

- [ ] use tunnel client state to actually connect to tunnel
  - load tunnel config from config.json
  - if tunnel config exists, start tunnel client
  - proxy requests from tunnel to local listener

## tunnel

- [ ] extract authentication and user management from tunnel
  - tunnel will become just a feature of a broader platform
  - OAuth2 flow, user management, and domain assignment should be separate service

- [ ] improve logging/debugging for tunnel end-to-end tests
  - capture all server logs (currently scattered in docker logs)
  - capture all client output (expectrl consumes it)
  - add structured logging with timestamps
  - show network activity (HTTP requests, DNS queries)
  - better error messages when assertions fail
  - consider using `docker-compose logs -f` approach
  - maybe expose server logs via volume mount

- [ ] switch back to CNAME records (nicer than A records)
  - current workaround uses A records pointing to server IP
  - CNAME would be cleaner but hickory-server doesn't handle CNAME resolution correctly
  - need to figure out how to make hickory-server include target A record in ANSWER section
  - options: custom response handler, different authority implementation, or wait for hickory v0.26

- [ ] standardize logging and code style
  - use tracing::info! everywhere instead of println! (for timestamps)
  - consistent log message format
  - consistent error message format (lowercase start)
  - remove redundant comments
