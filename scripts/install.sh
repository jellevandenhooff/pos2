#!/bin/sh

set -eu

# This script installs the pos2 runtime. It creates a local data directory and
# then runs a docker container configured to use that data directory. It is
# probably quite broken; it would be nice to instead upload an installer binary
# as part of a github release and to then run that.

# Known problems:
# - Auto-update with /var/run/docker.sock is mandatory
# - Data directory is fixed
# - No support for non-standard docker.sock location (eg. with podman?)
# - Maybe the self-update can trigger too early?

DOCKER_NAME=pos2
DOCKER_IMAGE="${DOCKER_IMAGE:-ghcr.io/jellevandenhooff/pos2:main}"
DOCKER_SOCKET=/var/run/docker.sock
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

echo "Checking if $DOCKER_SOCKET exists."
if [ -S $DOCKER_SOCKET ]; then
	echo "Found $DOCKER_SOCKET, continuing."
else
	echo "Did not find $DOCKER_SOCKET, exiting."
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
echo "running $DOCKER_IMAGE with access to the $DOCKER_SOCKET."
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
echo "Starting docker container $DOCKER_NAME running $DOCKER_IMAGE."
docker run \
	--name $DOCKER_NAME \
	--restart always \
	-d \
	-v "$DATA_DIR:/data" \
	-v "$DOCKER_SOCKET:/var/run/docker.sock" \
	$DOCKER_IMAGE
echo "Created docker container. Now $DOCKER_NAME is ready for setup."
echo
echo "Starting setup command inside container"
docker exec \
	-it \
	$DOCKER_NAME \
	/app setup
