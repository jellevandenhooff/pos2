#!/usr/bin/env bash

set -ex

nix build .#packages.x86_64-linux.loaderImage .#packages.x86_64-linux.innerappPlainImage

LOADER="$(realpath result)"
rm result
INNER_APP="$(realpath result-1)"
rm result-1

echo "LOADER=$LOADER"
echo "INNER_APP=$INNER_APP"

LOADER_LOAD_RESULT="$(docker load -q < $LOADER)"
LOADER_IMAGE="${LOADER_LOAD_RESULT#"Loaded image: "}"

INNER_APP_LOAD_RESULT="$(docker load -q < $INNER_APP)"
INNER_APP_IMAGE="${INNER_APP_LOAD_RESULT#"Loaded image: "}"

docker tag $LOADER_IMAGE ghcr.io/jellevandenhooff/loader:experiment
docker rmi $LOADER_IMAGE
# docker push ghcr.io/jellevandenhooff/loader:experiment

docker tag $INNER_APP_IMAGE ghcr.io/jellevandenhooff/innerapp:experiment
docker rmi $INNER_APP_IMAGE
docker push ghcr.io/jellevandenhooff/innerapp:experiment

docker run --rm -it -e container=1 -v "$(pwd)/loaderdatavolume:/data" ghcr.io/jellevandenhooff/loader:experiment
