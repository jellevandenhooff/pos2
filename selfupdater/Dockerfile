FROM rust:1.89 AS build

WORKDIR /app

# build dependencies
RUN cargo new /app/selfupdater
COPY ./Cargo.toml ./Cargo.lock /app/selfupdater/
WORKDIR /app/selfupdater
RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --release

# copy source and build binary
COPY . /app/selfupdater
RUN --mount=type=cache,target=/usr/local/cargo/registry <<EOF
  set -e
  # update timestamps to force a new build (copy pasted... unclear if needed)
  touch ./src/main.rs
  cargo build --release
  # cargo strip (does not work... should it?)
EOF

# build  app
FROM debian:bookworm-slim AS app

RUN apt-get update && apt-get install -y ca-certificates
COPY --from=build /app/selfupdater/target/release/selfupdater /selfupdater

ARG TEST_HEALTHY
ARG TEST_VERSION
ENV TEST_VERSION=${TEST_VERSION}
ENV TEST_HEALTHY=${TEST_HEALTHY}

CMD ["/selfupdater"]
