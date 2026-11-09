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
        if [ "${CI:-}" = "true" ]; then
            echo "hashline-model: uv is required in CI and is not installed" >&2
            exit 1
        fi
        echo "hashline-model: skipped, uv is not installed"
        exit 0
    fi
    out=$(mktemp)
    trap 'rm -f "$out"' EXIT
    uv run --no-project --python 3.12 docs/spec/hashline/verify_model.py > "$out"
    cmp "$out" docs/spec/hashline/verification.json

web:
    pnpm -C web install --frozen-lockfile
    pnpm -C web build
    touch internal/webui/dist/.gitkeep

web-check:
    pnpm -C web exec biome ci
    pnpm -C web exec vitest run
    pnpm -C web exec svelte-check

cf-check:
    pnpm -C deploy/cloudflare install --frozen-lockfile
    pnpm -C deploy/cloudflare exec tsc --noEmit
    pnpm -C deploy/cloudflare exec biome ci .

dist:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    for target in linux/amd64 linux/arm64 darwin/amd64 darwin/arm64 windows/amd64 windows/arm64; do
        os=${target%/*}
        arch=${target#*/}
        ext=""
        if [ "$os" = windows ]; then ext=.exe; fi
        for bin in dal dalgona; do
            CGO_ENABLED=0 GOOS=$os GOARCH=$arch go build -trimpath -o "dist/$bin-$os-$arch$ext" "./cmd/$bin"
        done
    done

box-image: dist
    docker buildx build -f deploy/box/Dockerfile --platform linux/amd64,linux/arm64 -t dalgona-box:$(go run ./cmd/dalgona --version | cut -d' ' -f2) .

verify: fmt-check vet lint test vuln hashline-model web-check cf-check
