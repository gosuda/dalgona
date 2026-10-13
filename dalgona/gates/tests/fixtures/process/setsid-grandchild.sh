#!/bin/sh
set -eu
pid_file=$1
setsid sh -c 'printf "%s\n" "$$" > "$1"; exec sleep 600' sh "$pid_file" &
child=$!
wait "$child"
