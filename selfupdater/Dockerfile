FROM rust:1.90-trixie AS build

WORKDIR /app
COPY . /app

RUN mkdir -p /out

RUN \
  --mount=type=cache,target=/usr/local/cargo/registry \
  --mount=type=cache,target=/app/target \
  cargo build --release -p selfupdater && \
  cp /app/target/release/selfupdater /out/app

# build  app
FROM debian:trixie-slim AS app
RUN apt-get update && apt-get install -y ca-certificates
COPY --from=build /out/app /app

ARG TEST_HEALTHY
ARG TEST_VERSION
ENV TEST_VERSION=${TEST_VERSION}
ENV TEST_HEALTHY=${TEST_HEALTHY}

ENTRYPOINT ["/app"]
CMD []
