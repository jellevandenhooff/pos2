#!/bin/sh
docker ps -a --filter "name=selfupdater*" --format "table {{.ID}}\t{{.Image}}\t{{.Names}}\t{{.Status}}"
