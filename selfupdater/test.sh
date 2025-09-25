#!/bin/bash

docker build . -t selfupdater:testing
docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock selfupdater:testing
