#!/usr/bin/env bash

set -ev

# Clean up old data to ensure fresh start
rm -rf data/dockerloader

# Build dockerloader supervisor
docker build -t dockerloader:testing -f Dockerfile.dockerloader .

# Build and push wasi3experiment application
docker build -t localhost:5050/wasi3experiment:testing -f Dockerfile.wasi3experiment .
docker push localhost:5050/wasi3experiment:testing

# Stop and remove old containers
docker ps --filter name="wasi3experiment*" -aq | xargs -r docker stop -s SIGKILL
docker ps --filter name="wasi3experiment*" -aq | xargs -r docker rm

# Run dockerloader supervisor (no Docker socket mount needed)
docker run --name=wasi3experiment --restart always \
  -e DOCKERLOADER_TARGET=host.docker.internal:5050/wasi3experiment:testing \
  -e DOCKERLOADER_UPDATE_INTERVAL_SECS=5 \
  -v "$(pwd)/data:/data" \
  dockerloader:testing
