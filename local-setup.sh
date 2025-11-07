#!/bin/bash
# setup local testing infrastructure
set -e

echo "local testing setup"
echo ""

# stop existing setup if running
echo "stopping existing containers..."
docker compose down 2>/dev/null || true
docker stop pebble-test 2>/dev/null || true
docker rm pebble-test 2>/dev/null || true

# start infrastructure using docker compose
echo "starting infrastructure..."
docker compose up -d

# wait for pebble to be ready
echo "waiting for pebble to start..."
for i in {1..10}; do
    if curl -k https://localhost:14000/dir 2>/dev/null | grep -q "newAccount"; then
        echo "✓ pebble is ready"
        break
    fi
    if [ $i -eq 10 ]; then
        echo "✗ pebble failed to start"
        docker compose logs
        exit 1
    fi
    sleep 1
done

# fetch and store CA certificates
echo ""
echo "fetching CA certificates..."
mkdir -p .test-certs
curl -k https://localhost:15000/roots/0 > .test-certs/pebble-acme-ca.crt
docker cp pebble:/test/certs/pebble.minica.pem .test-certs/pebble-https-ca.crt

echo "✓ CA certificates saved to .test-certs/"
echo ""
echo "setup complete"
echo ""
echo "pebble is running and accessible at:"
echo "  - ACME directory: https://localhost:14000/dir"
echo "  - management:     https://localhost:15000/roots/0"
echo ""
echo "CA certificates are stored in .test-certs/"
echo ""
echo "to stop: docker compose down"
