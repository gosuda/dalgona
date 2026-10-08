#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
cleanup() { :; }
trap cleanup EXIT INT TERM
script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/release.py" semver "$@"
