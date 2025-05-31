#!/bin/sh
set -ex
rm -rf wit_world
componentize-py -d ../wit bindings .
