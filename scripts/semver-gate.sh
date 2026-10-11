#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
cleanup() { :; }
trap cleanup EXIT INT TERM
script_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
release_py="$script_dir/release.py"
if command -v cygpath >/dev/null 2>&1; then
  release_py="$(cygpath -w "$release_py")"
fi
python=""
for candidate in python3 python py; do
  if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import sys' >/dev/null 2>&1; then
    python="$candidate"
    break
  fi
done
if [ -z "$python" ]; then
  echo "release.py needs Python 3 on PATH; install it and retry" >&2
  exit 64
fi
exec "$python" "$release_py" semver "$@"
