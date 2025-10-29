#!/usr/bin/env bash

set -ex

docker build -t tunnel:testing -f Dockerfile.tunnel .
docker run --rm -it -v "$(pwd)/tunnel/testing/client:/data" tunnel:testing
