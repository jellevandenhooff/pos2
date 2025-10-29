#!/usr/bin/env bash

set -ex

docker build -t wasi3experiment:latest -f Dockerfile.wasi3experiment .
docker run --rm -it -v "$(pwd)/wasi3experiment/apps:/data/apps" -v "$(pwd)/tunnel/testing/client:/data/client" wasi3experiment:latest
