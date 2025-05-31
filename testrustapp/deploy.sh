#!/bin/sh
set -ex
(cd ../server && ./run-release.sh publish ../testrustapp/app.json)
