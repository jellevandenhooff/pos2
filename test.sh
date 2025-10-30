#!/usr/bin/env bash

set -ev

docker build -t localhost:5050/wasi3experiment:testing -f Dockerfile.wasi3experiment .
docker push localhost:5050/wasi3experiment:testing

docker ps --filter name="wasi3experiment*" -aq | xargs -r docker stop -s SIGKILL
docker ps --filter name="wasi3experiment*" -aq | xargs -r docker rm

docker run --name=wasi3experiment --restart always -it -v /var/run/docker.sock:/var/run/docker.sock -v "$(pwd)/wasi3experiment/apps:/data/apps" -v "$(pwd)/tunnel/testing/client:/data/client" localhost:5050/wasi3experiment:testing
