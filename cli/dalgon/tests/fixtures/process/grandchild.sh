#!/bin/sh
set -eu
sleep 300 &
child=$!
printf '%s\n' "$child" > "$1"
wait "$child"
