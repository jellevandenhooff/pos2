#!/bin/sh

set -eu

DOCKER_NAME=pos2
DOCKER_IMAGE=ghcr.io/jellevandenhooff/pos2:main
DATA_DIR=$HOME/pos2data

echo "Checking if docker exists."
if [[ $(which docker) && $(docker --version) ]]; then
	echo "Found docker, continuing."
else
	echo "Did not find docker cli, exiting."
	exit 1
fi

echo "Checking if a pos2 container exists."
if [ "$(docker ps -aq -f name='^'$DOCKER_NAME'$')" ]; then
	echo "Container already exists, exiting."
	echo "If the container is not working, you can stop and remove the container by running 'docker stop $DOCKER_NAME && docker rm $DOCKER_NAME'. Afterwards, you can rerun this script."
	# TODO: provide command to run configure inside container?
	exit 0
fi

echo "This script will create a directory $DATA_DIR and then start a docker container with access to the /var/run/docker.sock."

read -p "Do you want to continue? [y/n] " -r
echo    # (optional) move to a new line
if [[ $REPLY =~ ^[Yy]$ ]]; then
	echo "Got confirmation."
else
	echo "Did not get confirmation, exiting."
fi

echo "Creating data directory $DATA_DIR."
mkdir -p $DATA_DIR

echo "Starting docker container."
docker run \
	--name $DOCKER_NAME \
	--restart always \
	-v "$DATA_DIR:/data" \
	-v "/var/run/docker.sock:/var/run/docker.sock" \
	$DOCKER_IMAGE

echo "TODO: jump into configuration"
