#!/bin/bash

set -ex

docker build -t dockerloader:testing -f Dockerfile.dockerloader .

echo "Building and pushing version 1"
docker build -t localhost:5050/dockerloaded:testing -f Dockerfile.dockerloaded --build-arg VERSION=1.0 .
docker push localhost:5050/dockerloaded:testing

rm -rf $(pwd)/data/dockerloader

echo "First run - should download and run version 1"
docker run --rm -v "$(pwd)/data/dockerloader:/data" dockerloader:testing
