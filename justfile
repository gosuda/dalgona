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

[positional-arguments]
cf-pin tag:
    #!/usr/bin/env bash
    set -euo pipefail
    tag="$1"
    if [[ ! "$tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
        echo "cf-pin: invalid tag: $tag" >&2
        exit 1
    fi
    ref="ghcr.io/gosuda/dalgona-box:$tag"
    open=$(printf '\173\173')
    close=$(printf '\175\175')
    if ! digest=$(docker buildx imagetools inspect "$ref" --format "$open.Manifest.Digest$close" 2>&1); then
        echo "cf-pin: cannot inspect $ref:" >&2
        echo "$digest" >&2
        exit 1
    fi
    if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
        echo "cf-pin: bad digest for $ref: $digest" >&2
        exit 1
    fi
    IMAGE="ghcr.io/gosuda/dalgona-box@$digest" python3 - <<'EOF'
    import io, os
    path = "{{justfile_directory()}}/deploy/cloudflare/wrangler.jsonc"
    image = os.environ["IMAGE"]
    with io.open(path, encoding="utf-8") as f:
        lines = f.readlines()
    hits = [i for i, line in enumerate(lines) if "ghcr.io/gosuda/dalgona-box" in line]
    if len(hits) != 1:
        raise SystemExit("expected one box image line, found %d" % len(hits))
    lines[hits[0]] = '      "image": "%s",\n' % image
    with io.open(path, "w", encoding="utf-8", newline="\n") as f:
        f.writelines(lines)
    EOF
    just cf-deploy-check
    echo "cf-pin: $ref pinned"

cf-deploy-check:
    grep -Eq '"image": "ghcr.io/gosuda/dalgona-box@sha256:[0-9a-f]{64}"' {{justfile_directory()}}/deploy/cloudflare/wrangler.jsonc || (echo "cf-deploy-check: wrangler.jsonc image must be a digest pin; run: just cf-pin <tag>" >&2; exit 1)

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
