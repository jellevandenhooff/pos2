#!/bin/bash

docker build . -t localhost:5050/selfupdater:testing
docker push localhost:5050/selfupdater:testing
