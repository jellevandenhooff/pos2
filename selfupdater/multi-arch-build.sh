#!/bin/bash

set -ev

docker build \
  --push \
  --platform linux/arm64/v8,linux/amd64 \
  --tag localhost:5050/selfupdater:testing .
