#!/bin/bash

set -v

docker ps --filter name="selfupdater*" -aq | xargs docker stop -s SIGKILL
docker ps --filter name="selfupdater*" -aq | xargs docker rm

docker build . -t localhost:5050/selfupdater:testing
docker push localhost:5050/selfupdater:testing

rm ./data/selfupdater.sqlite3

docker run --name=selfupdater -it -v /var/run/docker.sock:/var/run/docker.sock -v "$(pwd)/data:/data" localhost:5050/selfupdater:testing
