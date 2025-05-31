#!/bin/sh
set -ex

go install github.com/caddyserver/xcaddy/cmd/xcaddy@latest
xcaddy build v2.9.1 \
    --with github.com/caddy-dns/digitalocean@master

