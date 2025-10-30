#!/bin/bash

set -ev

docker build -t localhost:5050/wasi3experiment:testing -f Dockerfile.wasi3experiment .
docker push localhost:5050/wasi3experiment:testing
