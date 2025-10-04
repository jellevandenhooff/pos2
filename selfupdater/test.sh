#!/bin/bash

set -ev

docker ps --filter name="selfupdater*" -aq | xargs -r docker stop -s SIGKILL
docker ps --filter name="selfupdater*" -aq | xargs -r docker rm

docker build . -t localhost:5050/selfupdater:testing
docker push localhost:5050/selfupdater:testing

rm ./data/selfupdater.sqlite3

docker run --name=selfupdater --restart always -it -v /var/run/docker.sock:/var/run/docker.sock -v "$(pwd)/data:/data" localhost:5050/selfupdater:testing
