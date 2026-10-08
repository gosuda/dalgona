#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
cleanup() { :; }
trap cleanup EXIT INT TERM
script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
python="$(bash "$script_dir/release-python.sh")"
exec "$python" "$script_dir/release.py" publish "$@"
