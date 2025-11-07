#!/bin/sh

set -eu

# This script installs the pos2 runtime. It creates a local data directory and
# then runs a docker container configured to use that data directory.

DOCKER_NAME=pos2
SUPERVISOR_IMAGE="${SUPERVISOR_IMAGE:-ghcr.io/jellevandenhooff/pos2:supervisor-main}"
RUNTIME_IMAGE="${RUNTIME_IMAGE:-ghcr.io/jellevandenhooff/pos2:runtime-main}"
UPDATE_INTERVAL="${UPDATE_INTERVAL:-300}"
DATA_DIR=$HOME/pos2data
export DOCKER_CLI_HINTS=false

echo
echo "Welcome to the pos2 installer. This is extremely unstable software. Enjoy!"
echo
echo "The installer will first do some checks to see if docker works."
echo "Checking if docker binary exists."
if [[ $(which docker) && $(docker --version) ]]; then
	echo "Found docker, continuing."
else
	echo "Did not find docker, exiting."
	exit 1
fi


echo "Checking if a $DOCKER_NAME container exists."
if [ "$(docker ps -aq -f name='^'$DOCKER_NAME'$')" ]; then
	echo "Container already exists, exiting."
	echo "If the container is not working, you can stop and remove the container"
	echo "by running 'docker stop $DOCKER_NAME && docker rm $DOCKER_NAME'."
	echo "Afterwards, you can rerun this script."
	# TODO: provide command to run configure inside container?
	exit 1
fi

echo
echo "The installer is ready to continue. if you confirm, the installer"
echo "will create a directory $DATA_DIR and then start a docker container"
echo "running $SUPERVISOR_IMAGE."
echo

read -p "Do you want to continue? [y/n] " -r
if [[ $REPLY =~ ^[Yy]$ ]]; then
	echo "Got confirmation."
else
	echo "Did not get confirmation, exiting."
	exit 1
fi

echo
echo "Creating data directory $DATA_DIR."
mkdir -p $DATA_DIR
if [ -d "$DATA_DIR/dockerloader" ]; then
	echo "Note: $DATA_DIR/dockerloader already exists and will be reused."
	echo "If you want a fresh install, remove it first with: rm -rf $DATA_DIR/dockerloader"
fi
echo "Pulling supervisor image $SUPERVISOR_IMAGE."
docker pull $SUPERVISOR_IMAGE
echo "Starting docker container $DOCKER_NAME."
docker run \
	--name $DOCKER_NAME \
	--restart always \
	-d \
	-e "DOCKERLOADER_TARGET=$RUNTIME_IMAGE" \
	-e "DOCKERLOADER_UPDATE_INTERVAL_SECS=$UPDATE_INTERVAL" \
	-v "$DATA_DIR:/data" \
	$SUPERVISOR_IMAGE
echo "Created docker container $DOCKER_NAME."
echo
echo "The supervisor is now running and will download the runtime."
echo "Waiting for runtime to be ready..."
if docker exec $DOCKER_NAME cli setup; then
	echo
	echo "Setup completed successfully!"
else
	echo
	echo "Setup encountered an error. Check logs with: docker logs $DOCKER_NAME"
	exit 1
fi
echo
echo "Installation complete! You can view logs with: docker logs -f $DOCKER_NAME"
