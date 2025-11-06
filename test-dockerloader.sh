#!/bin/bash

set -ex

# Build the dockerloader bootstrap image
docker build -t dockerloader:testing -f Dockerfile.dockerloader .

# Build and push version 1
echo "=== Building and pushing version 1 ==="
docker build -t localhost:5050/dockerloaded:testing -f Dockerfile.dockerloaded --build-arg VERSION=1.0 .
docker push localhost:5050/dockerloaded:testing

# Clean up any existing data
rm -rf $(pwd)/data/dockerloader

# Run dockerloader - should download and run version 1
echo "=== First run - should download version 1 ==="
docker run --rm -v "$(pwd)/data/dockerloader:/data" dockerloader:testing

# Build and push version 2
echo "=== Building and pushing version 2 ==="
docker build -t localhost:5050/dockerloaded:testing -f Dockerfile.dockerloaded --build-arg VERSION=2.0 .
docker push localhost:5050/dockerloaded:testing

# Run dockerloader again - should detect update and download version 2
echo "=== Second run - should detect update and download version 2 ==="
docker run --rm -v "$(pwd)/data/dockerloader:/data" dockerloader:testing

# Run dockerloader one more time - should run version 2 without updating
echo "=== Third run - should run version 2 without updating ==="
docker run --rm -v "$(pwd)/data/dockerloader:/data" dockerloader:testing

echo "=== Test complete! ==="
