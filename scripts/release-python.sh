#!/usr/bin/env bash
# Print a python3 interpreter able to run release.py (tomllib, Python >= 3.11).
set -euo pipefail
IFS=$'\n\t'
for candidate in python3 python3.14 python3.13 python3.12 python3.11; do
    if command -v "$candidate" >/dev/null 2>&1 \
        && "$candidate" -c 'import tomllib' >/dev/null 2>&1; then
        printf '%s\n' "$candidate"
        exit 0
    fi
done
echo "scripts/release.py needs Python 3.11 or newer; no python3 with tomllib found on PATH" >&2
exit 64
