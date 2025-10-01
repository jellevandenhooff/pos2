#!/bin/bash

set -ev

docker build --build-arg TEST_HEALTHY="$TEST_HEALTHY" --build-arg TEST_VERSION="$TEST_VERSION" . -t localhost:5050/selfupdater:testing
docker push localhost:5050/selfupdater:testing
