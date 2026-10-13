#!/bin/sh
set -eu
printf '%s\n' "$$" > "$1"
exec sleep 600
