fmt-check:
    go tool golangci-lint fmt --diff

vet:
    go vet ./...

lint:
    go tool golangci-lint run ./...

test:
    go test -race -count=1 ./...

vuln:
    go tool govulncheck ./...

hashline-model:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v uv >/dev/null; then
        echo "hashline-model: skipped, uv is not installed"
        exit 0
    fi
    out=$(mktemp)
    trap 'rm -f "$out"' EXIT
    uv run --no-project --python 3.12 docs/spec/hashline/verify_model.py > "$out"
    cmp "$out" docs/spec/hashline/verification.json

verify: fmt-check vet lint test vuln hashline-model
