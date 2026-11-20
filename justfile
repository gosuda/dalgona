build:
    go build -trimpath -o bin/ ./cmd/...

[no-cd, no-exit-message, positional-arguments]
dal *args:
    @go build -C "{{justfile_directory()}}" -trimpath -o bin/ ./cmd/dal
    @exec "{{justfile_directory()}}/bin/dal" "$@"

[no-cd, no-exit-message, positional-arguments]
dalgona *args:
    @go build -C "{{justfile_directory()}}" -trimpath -o bin/ ./cmd/dalgona
    @exec "{{justfile_directory()}}/bin/dalgona" "$@"

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
    uv run --no-project --python 3.15 docs/dal/spec/hashline/verify_model.py > "$out"
    cmp "$out" docs/dal/spec/hashline/verification.json

spec-style:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'status=$?; [ "$status" -eq 0 ] || echo "spec-style: failed with status $status" >&2' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if ! command -v uv >/dev/null; then
        if [ "${CI:-}" = "true" ]; then
            echo "spec-style: uv is required in CI and is not installed" >&2
            exit 1
        fi
        echo "spec-style: skipped, uv is not installed"
        exit 0
    fi
    uv run --no-project --python 3.15 docs/dal/spec/check_style.py docs/dal/spec
    uv run --no-project --python 3.15 docs/dal/spec/check_style.py docs/dalgona/spec
    uv run --no-project --python 3.15 docs/dal/spec/check_style_test.py

docs-links:
    python3 docs/check_links.py

web:
    pnpm -C web install --frozen-lockfile
    pnpm -C web build
    touch internal/dalgona/webui/dist/.gitkeep

web-check:
    pnpm -C web exec biome ci
    pnpm -C web exec vitest run
    pnpm -C web exec svelte-check

cf-check:
    pnpm -C deploy/cloudflare install --frozen-lockfile
    pnpm -C deploy/cloudflare exec tsc --noEmit
    pnpm -C deploy/cloudflare exec tsc --noEmit -p tsconfig.test.json
    pnpm -C deploy/cloudflare exec biome ci .
    pnpm -C deploy/cloudflare test

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
    cp THIRD_PARTY_NOTICES.md dist/
    overlay_args=()
    if [ -n "${DALGONA_REPORT_CLIENT_ID:-}" ]; then
        overlay_dir="$(mktemp -d)"
        printf 'package report\n\nconst reportAppClientID = "%s"\n' "$DALGONA_REPORT_CLIENT_ID" > "$overlay_dir/clientid.go"
        clientid_path="$(cd "{{justfile_directory()}}/internal/dalgona/report" && pwd)/clientid.go"
        printf '{"Replace": {"%s": "%s"}}\n' "$clientid_path" "$overlay_dir/clientid.go" > "$overlay_dir/overlay.json"
        overlay_args+=(-overlay "$overlay_dir/overlay.json")
    fi
    for target in linux/amd64 linux/arm64 darwin/amd64 darwin/arm64 windows/amd64 windows/arm64; do
        os=${target%/*}
        arch=${target#*/}
        ext=""
        if [ "$os" = windows ]; then ext=.exe; fi
        for bin in dal dalgona; do
            CGO_ENABLED=0 GOOS=$os GOARCH=$arch go build "${overlay_args[@]}" -trimpath -o "dist/$bin-$os-$arch$ext" "./cmd/$bin"
        done
    done

box-dist:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    cp THIRD_PARTY_NOTICES.md dist/
    for arch in amd64 arm64; do
        CGO_ENABLED=0 GOOS=linux GOARCH=$arch go build -trimpath -o "dist/dalgona-linux-$arch" ./cmd/dalgona
    done

box_tag := `git describe --tags --exact-match HEAD 2>/dev/null || git rev-parse --short=12 HEAD`

box-image: box-dist
    docker buildx build -f deploy/box/Dockerfile --platform linux/amd64,linux/arm64 -t dalgona-box:{{box_tag}} .

verify-go: fmt-check vet lint test vuln

verify-shared: hashline-model spec-style docs-links web-check cf-check

verify: verify-go verify-shared

live_root_dal := "/tmp/dg-dal"
live_root_dalgona := "/tmp/dg-dalgona"

omp-tui-install:
    #!/usr/bin/env bash
    set -euo pipefail
    src=.references/oh-my-pi/.omp/tools
    for f in tui.ts package.json bun.lock; do
        test -f "$src/$f" || { echo "omp-tui-install: missing $src/$f" >&2; exit 1; }
    done
    command -v bun >/dev/null || { echo "omp-tui-install: bun is required" >&2; exit 1; }
    mkdir -p .omp/tools
    cp "$src/tui.ts" "$src/package.json" "$src/bun.lock" .omp/tools/
    (cd .omp/tools && bun install --frozen-lockfile)
    echo "omp-tui-install: run /restart in omp to load the tui tool"

live-up-dal:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{quote(live_root_dal)}}
    source internal/llmstub/live-workspace.sh
    git config --global user.name >/dev/null 2>&1 || { echo "live-up-dal: git global user.name is not set; run git config --global user.name <name>" >&2; exit 1; }
    git config --global user.email >/dev/null 2>&1 || { echo "live-up-dal: git global user.email is not set; run git config --global user.email <email>" >&2; exit 1; }
    prepare_live_workspace "$w" dal
    trap 'finish_live_start "$?" "$w" dal' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    mkdir -p "$w/bin" "$w/dal" "$w/fakehome" "$w/ws"
    chmod 700 "$w/dal"
    go build -trimpath -o "$w/bin/" ./cmd/dal ./internal/llmstub/cmd/llmstub
    cp -R internal/dal/e2e/testdata/workspace/. "$w/ws/"
    git -C "$w/ws" init -q
    git -C "$w/ws" add -A
    git -C "$w/ws" commit -qm seed
    printf '%s\n' 'model = "local/qwen-test"' 'approval = "ask"' 'edit_style = "apply_patch"' '' '[approval_decider]' 'decider = "llm:local/qwen-test"' '' '[providers.local]' \
        'base_url = "http://127.0.0.1:18080/v1"' 'api_key_env = "LOCAL_LLM_KEY"' \
        '' '[providers.openai]' 'base_url = "http://127.0.0.1:18080/v1"' \
        '[models."local/qwen-test"]' 'context_window = 32000' > "$w/dal/config.toml"
    printf 'export HOME=%q DAL_HOME=%q LOCAL_LLM_KEY=sk-live-test\n' "$w/fakehome" "$w/dal" > "$w/env"
    nohup "$w/bin/llmstub" -listen 127.0.0.1:18080 -script internal/dal/e2e/testdata/live.json \
        -key sk-live-test -record "$w/requests.jsonl" >/dev/null 2>"$w/llmstub.log" &
    echo $! > "$w/llmstub.pid"
    for _ in $(seq 50); do grep -q '^llmstub: serving ' "$w/llmstub.log" && break; sleep 0.1; done
    grep -qx 'llmstub: serving http://127.0.0.1:18080/v1' "$w/llmstub.log" || { echo "live-up-dal: llmstub did not start; see $w/llmstub.log" >&2; exit 1; }
    echo "live-up-dal: env $w/env"

live-up-dalgona:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{quote(live_root_dalgona)}}
    source internal/llmstub/live-workspace.sh
    git config --global user.name >/dev/null 2>&1 || { echo "live-up-dalgona: git global user.name is not set; run git config --global user.name <name>" >&2; exit 1; }
    git config --global user.email >/dev/null 2>&1 || { echo "live-up-dalgona: git global user.email is not set; run git config --global user.email <email>" >&2; exit 1; }
    prepare_live_workspace "$w" dalgona
    trap 'finish_live_start "$?" "$w" dalgona' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    mkdir -p "$w/bin" "$w/dalgona" "$w/fakehome" "$w/ws"
    chmod 700 "$w/dalgona"
    go build -trimpath -o "$w/bin/" ./cmd/dalgona ./internal/llmstub/cmd/llmstub
    cp -R internal/dalgona/e2e/testdata/workspace/. "$w/ws/"
    git -C "$w/ws" init -q
    git -C "$w/ws" add -A
    git -C "$w/ws" commit -qm seed
    printf '%s\n' 'model = "local/qwen-test"' 'approval = "ask"' 'edit_style = "apply_patch"' '' '[approval_decider]' 'decider = "llm:local/qwen-test"' '' '[providers.local]' \
        'base_url = "http://127.0.0.1:18081/v1"' 'api_key_env = "LOCAL_LLM_KEY"' \
        '' '[providers.openai]' 'base_url = "http://127.0.0.1:18081/v1"' \
        '[models."local/qwen-test"]' 'context_window = 32000' > "$w/dalgona/config.toml"
    printf '%s\n' '' '[edit_review]' 'feedback = ["lost_code"]' >> "$w/dalgona/config.toml"
    printf 'export HOME=%q DAL_HOME=%q DALGONA_HOME=%q LOCAL_LLM_KEY=sk-live-test\n' "$w/fakehome" "$w/dalgona" "$w/dalgona" > "$w/env"
    base=(env -i PATH="$PATH" HOME="$w/fakehome" DAL_HOME="$w/dalgona" DALGONA_HOME="$w/dalgona" LOCAL_LLM_KEY=sk-live-test)
    nohup "$w/bin/llmstub" -listen 127.0.0.1:18081 -script internal/dalgona/e2e/testdata/live.json \
        -key sk-live-test -record "$w/requests.jsonl" >/dev/null 2>"$w/llmstub.log" &
    echo $! > "$w/llmstub.pid"
    for _ in $(seq 50); do grep -q '^llmstub: serving ' "$w/llmstub.log" && break; sleep 0.1; done
    grep -qx 'llmstub: serving http://127.0.0.1:18081/v1' "$w/llmstub.log" || { echo "live-up-dalgona: llmstub did not start; see $w/llmstub.log" >&2; exit 1; }
    (cd "$w/ws" && exec "${base[@]}" "$w/bin/dalgona" daemon >/dev/null 2>"$w/daemon.stderr") &
    echo $! > "$w/daemon.pid"
    for _ in $(seq 50); do test -S "$w/dalgona/dalgona.sock" && break; sleep 0.1; done
    test -S "$w/dalgona/dalgona.sock" || { echo "live-up-dalgona: daemon did not start; see $w/daemon.stderr" >&2; exit 1; }
    (cd "$w/ws" && exec "${base[@]}" "$w/bin/dalgona" web >/dev/null 2>"$w/web.log") &
    echo $! > "$w/web.pid"
    re='^http://127\.0\.0\.1:[0-9]+/#token=[A-Za-z0-9_-]+$'
    for _ in $(seq 100); do grep -Eq "$re" "$w/web.log" && break; sleep 0.1; done
    grep -E "$re" "$w/web.log" > "$w/web.url" || { echo "live-up-dalgona: web did not start; see $w/web.log" >&2; exit 1; }
    echo "live-up-dalgona: env $w/env; web $(cat "$w/web.url")"

live-down-dal:
    #!/usr/bin/env bash
    set -uo pipefail
    w={{quote(live_root_dal)}}
    source internal/llmstub/live-workspace.sh
    status=0
    for name in llmstub; do
        f="$w/$name.pid"; [ -e "$f" ] || continue
        pid=$(cat "$f")
        cmd=$(tr '\0' ' ' 2>/dev/null < "/proc/$pid/cmdline" || true)
        case "$cmd" in "$w/bin/"*) kill -TERM "$pid" ;; *) rm -f "$f"; continue ;; esac
        for _ in $(seq 60); do live_pid_running "$pid" || break; sleep 0.1; done
        if live_pid_running "$pid"; then echo "live-down-dal: $name pid $pid still running after 6 s" >&2; status=1; else rm -f "$f"; fi
    done
    exit $status

live-down-dalgona:
    #!/usr/bin/env bash
    set -uo pipefail
    w={{quote(live_root_dalgona)}}
    source internal/llmstub/live-workspace.sh
    status=0
    for name in web daemon llmstub; do
        f="$w/$name.pid"; [ -e "$f" ] || continue
        pid=$(cat "$f")
        cmd=$(tr '\0' ' ' 2>/dev/null < "/proc/$pid/cmdline" || true)
        case "$cmd" in "$w/bin/"*) kill -TERM "$pid" ;; *) rm -f "$f"; continue ;; esac
        for _ in $(seq 60); do live_pid_running "$pid" || break; sleep 0.1; done
        if live_pid_running "$pid"; then echo "live-down-dalgona: $name pid $pid still running after 6 s" >&2; status=1; else rm -f "$f"; fi
    done
    pkill -TERM -x -f "$w/bin/dalgona daemon" || true
    tmux -L dglive kill-server 2>/dev/null || true
    for _ in $(seq 50); do test -S "$w/dalgona/dalgona.sock" || break; sleep 0.1; done
    if test -S "$w/dalgona/dalgona.sock"; then echo "live-down-dalgona: $w/dalgona/dalgona.sock remains" >&2; status=1; fi
    exit $status

live-check-dal:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{quote(live_root_dal)}}
    source internal/llmstub/live-workspace.sh
    start_live_check "$w" dal
    source "$w/env"
    (cd "$w/ws" && "$w/bin/dal" -p "read a.txt" --json </dev/null) > "$w/check-dal.json"
    jq -e -s 'length >= 2 and all(.[]; .schemaVersion == 1) and (.[-1] | .type == "result" and .status == "completed" and .stop_reason == "stop") and all(.[:-1][]; .type == "item" and (.item | type == "object"))' "$w/check-dal.json" >/dev/null
    test "$(grep -c '"turn":"t-read-2"' "$w/requests.jsonl")" = 1
    test "$(grep -c 'hello from a.txt' "$w/requests.jsonl")" -ge 1
    ! grep -q '"turn":"unmatched"' "$w/requests.jsonl"
    echo "live-check-dal: ok"

live-check-dalgona:
    #!/usr/bin/env bash
    set -euo pipefail
    w={{quote(live_root_dalgona)}}
    source internal/llmstub/live-workspace.sh
    start_live_check "$w" dalgona
    source "$w/env"
    (cd "$w/ws" && "$w/bin/dalgona" -p "read a.txt" --json </dev/null) > "$w/check-dalgona.json"
    jq -e -s 'length >= 2 and all(.[]; .schemaVersion == 1) and (.[-1] | .type == "result" and .status == "completed" and .stop_reason == "stop") and all(.[:-1][]; .type == "item" and (.item | type == "object"))' "$w/check-dalgona.json" >/dev/null
    test "$(grep -c '"turn":"t-read-2"' "$w/requests.jsonl")" = 1
    test "$(grep -c 'hello from a.txt' "$w/requests.jsonl")" -ge 1
    ! grep -q '"turn":"unmatched"' "$w/requests.jsonl"
    base=$(sed 's|/#token=.*||' "$w/web.url")
    test "$(curl -s -o /dev/null -w '%{http_code}' "$base/rpc")" = 401
    test "$(curl -s -o /dev/null -w '%{http_code}' "$base/")" = 200 || { echo "live-check-dalgona: web UI not built; run just web" >&2; exit 1; }
    echo "live-check-dalgona: ok"
