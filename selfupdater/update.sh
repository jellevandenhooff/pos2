#!/bin/bash

set -ev

docker build -q --build-arg TEST_HEALTHY="$TEST_HEALTHY" --build-arg TEST_VERSION="$TEST_VERSION" . -t localhost:5050/selfupdater:testing
docker push -q localhost:5050/selfupdater:testing
