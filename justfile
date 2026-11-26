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
    command -v uv >/dev/null || { echo "hashline-model: uv is required to run verify_model.py with Python 3.15" >&2; exit 1; }
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
    command -v uv >/dev/null || { echo "spec-style: uv is required to run the specification checks with Python 3.15" >&2; exit 1; }
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

[private]
desktop-tags:
    #!/usr/bin/env bash
    set -euo pipefail
    tags=gui
    if [ "$(go env GOOS)" != linux ]; then
        echo "$tags"
        exit 0
    fi
    pc="${PKG_CONFIG:-pkg-config}"
    dropped=()
    if ! "$pc" --exists wayland-client wayland-cursor wayland-egl egl xkbcommon; then
        tags+=,nowayland
        dropped+=("Wayland: $pc finds no wayland-client, wayland-cursor, wayland-egl, egl or xkbcommon")
    fi
    if ! "$pc" --exists x11 xkbcommon xkbcommon-x11 x11-xcb xcursor xfixes egl; then
        tags+=,nox11
        dropped+=("X11: $pc finds no x11, xkbcommon, xkbcommon-x11, x11-xcb, xcursor, xfixes or egl")
    fi
    if ! printf '#include <vulkan/vulkan.h>\n' | "${CC:-cc}" -E -x c - >/dev/null 2>&1; then
        tags+=,novulkan
        dropped+=("Vulkan: vulkan/vulkan.h is absent, so only the OpenGL ES renderer builds")
    fi
    for reason in "${dropped[@]}"; do
        echo "desktop: building without $reason" >&2
    done
    case ",$tags," in
        *,nowayland,*nox11,*)
            echo "desktop: no Linux window backend can build; install the Gio Linux packages or set PKG_CONFIG to the system pkg-config" >&2
            exit 1
            ;;
    esac
    if [ "${#dropped[@]}" -gt 0 ] && [ "${CI:-}" = "true" ]; then
        echo "desktop: CI requires the Wayland and X11 backends and the Vulkan renderer" >&2
        exit 1
    fi
    echo "$tags"

desktop-build:
    #!/usr/bin/env bash
    set -euo pipefail
    tags=$("{{just_executable()}}" --justfile "{{justfile()}}" desktop-tags)
    mkdir -p bin
    go build -tags "$tags" -trimpath -o bin/ ./cmd/dalgona-gui
    echo "desktop-build: bin/dalgona-gui built with tags $tags"

desktop-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tags=$("{{just_executable()}}" --justfile "{{justfile()}}" desktop-tags)
    echo "desktop-check: tags $tags"
    go tool golangci-lint fmt --diff
    go vet -tags "$tags" ./...
    go tool golangci-lint run --build-tags "$tags" ./...
    go test -race -count=1 -tags "$tags" ./cmd/dalgona-gui/... ./internal/dalgona/gui/...
    go tool govulncheck -tags "$tags" ./...
    (cd internal/dalgona/gui/platform/fontfaces && sha256sum --check --quiet SHA256SUMS)
    if go list -deps ./cmd/dal ./cmd/dalgona | grep -q '^gioui\.org'; then
        echo "desktop-check: a headless binary imports gioui.org" >&2
        exit 1
    fi
    go build -tags "$tags" -trimpath -o /dev/null ./cmd/dalgona-gui

[positional-arguments]
desktop-smoke backend:
    #!/usr/bin/env bash
    set -euo pipefail
    backend="$1"
    session=(env)
    case "$(go env GOOS)/$backend" in
        linux/wayland)
            [ -n "${WAYLAND_DISPLAY:-}" ] || { echo "desktop-smoke: the wayland run needs a Wayland session; WAYLAND_DISPLAY is not set" >&2; exit 1; }
            ;;
        linux/x11)
            [ -n "${DISPLAY:-}" ] || { echo "desktop-smoke: the x11 run needs an X11 display; DISPLAY is not set" >&2; exit 1; }
            session=(env -u WAYLAND_DISPLAY)
            ;;
        linux/*)
            echo "desktop-smoke: name the backend: just desktop-smoke wayland or just desktop-smoke x11" >&2
            exit 1
            ;;
    esac
    tags=$("{{just_executable()}}" --justfile "{{justfile()}}" desktop-tags)
    case ",$tags," in
        *,no"$backend",*) echo "desktop-smoke: this build has no $backend backend (tags $tags)" >&2; exit 1 ;;
    esac
    mkdir -p dist
    goos=$(go env GOOS)
    base="dist/desktop-smoke-$goos-$(go env GOARCH)-$backend"
    record="$base.txt"
    host="$base.host.json"
    json_str() { local s=$1; s=${s//\\/\\\\}; s=${s//\"/\\\"}; s=${s//$'\t'/\\t}; s=${s//$'\r'/\\r}; s=${s//$'\n'/\\n}; printf '"%s"' "$s"; }
    json_list() { local first=1 item; printf '['; while IFS= read -r item; do [ -n "$item" ] || continue; [ "$first" = 1 ] || printf ', '; json_str "$item"; first=0; done; printf ']'; }
    first_line() { "$@" 2>/dev/null | head -n 1 || true; }
    linux_packages() { dpkg-query -W -f='${db:Status-Abbrev} ${Package} ${Version}\n' "$@" 2>/dev/null | awk '$1 == "ii" { print $2 " " $3 }' || true; }
    win_query() { powershell.exe -NoProfile -Command "$1" 2>/dev/null | tr -d '\r' || true; }
    case "$goos" in
        linux)
            edition=$(. /etc/os-release && echo "${PRETTY_NAME:-}")
            build=$(. /etc/os-release && echo "${VERSION:-}")
            gpu=${DALGONA_HOST_GPU:-$(first_line sh -c "lspci | grep -E 'VGA|3D|Display' | cut -d: -f3- | sed 's/^ //'")}
            driver=${DALGONA_HOST_DRIVER:-$(first_line sh -c "glxinfo -B | sed -n 's/^OpenGL version string: //p'")}
            compositor=${DALGONA_COMPOSITOR:-$(first_line gnome-shell --version)}
            compositor=${compositor:-${XDG_CURRENT_DESKTOP:-}}
            portal=${DALGONA_PORTAL_BACKEND:-$(linux_packages xdg-desktop-portal xdg-desktop-portal-gnome xdg-desktop-portal-gtk xdg-desktop-portal-kde xdg-desktop-portal-wlr | paste -sd ',' -)}
            packages=$(linux_packages gnome-shell mutter xwayland labwc wayfire xdg-desktop-portal at-spi2-core ibus fcitx5 libgtk-3-0)
            input=${DALGONA_INPUT_METHOD:-$(first_line ibus version)}
            input=${input:-${GTK_IM_MODULE:-${XMODIFIERS:-}}}
            virtual=${DALGONA_DISPLAY_VIRTUAL:-}
            if [ "$backend" = x11 ] && [ -z "$virtual" ] && pgrep -a Xvfb 2>/dev/null | grep -q " ${DISPLAY%%.*}\b"; then virtual="Xvfb $(first_line sh -c "Xvfb -version 2>&1 | sed -n 's/^X.Org X Server //p'")"; fi
            ;;
        darwin)
            edition="$(sw_vers -productName) $(sw_vers -productVersion)"
            build=$(sw_vers -buildVersion)
            gpu=${DALGONA_HOST_GPU:-$(first_line sh -c "system_profiler SPDisplaysDataType | awk -F': ' '/Chipset Model/ { print \$2 }'")}
            driver=${DALGONA_HOST_DRIVER:-$(first_line sh -c "system_profiler SPDisplaysDataType | awk -F': ' '/Metal/ { print \$2 }'")}
            input=${DALGONA_INPUT_METHOD:-}
            virtual=${DALGONA_DISPLAY_VIRTUAL:-}
            ;;
        windows)
            edition=$(win_query "(Get-CimInstance Win32_OperatingSystem).Caption")
            build=$(win_query "\$v = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'; \"\$(\$v.DisplayVersion) \$(\$v.CurrentBuild).\$(\$v.UBR)\"")
            gpu=${DALGONA_HOST_GPU:-$(win_query "(Get-CimInstance Win32_VideoController | Select-Object -First 1).Name")}
            driver=${DALGONA_HOST_DRIVER:-$(win_query "(Get-CimInstance Win32_VideoController | Select-Object -First 1).DriverVersion")}
            input=${DALGONA_INPUT_METHOD:-}
            virtual=${DALGONA_DISPLAY_VIRTUAL:-}
            ;;
    esac
    updates=${DALGONA_HOST_UPDATES:-}
    if [ -z "$updates" ] && [ "$goos" = windows ]; then updates=$(win_query "Get-HotFix | ForEach-Object { \$_.HotFixID }"); fi
    locale=${LC_ALL:-${LC_CTYPE:-${LANG:-}}}
    missing=""
    for fact in "edition:$edition" "OS build:$build" "GPU (DALGONA_HOST_GPU):$gpu" "graphics driver (DALGONA_HOST_DRIVER):$driver" \
        "locale (LANG):$locale" "input method (DALGONA_INPUT_METHOD):$input" "installed updates (DALGONA_HOST_UPDATES, one per line):$updates" \
        "installation image digest (DALGONA_IMAGE_DIGEST=sha256:...):${DALGONA_IMAGE_DIGEST:-}"; do
        [ -n "${fact#*:}" ] || missing="$missing; ${fact%%:*}"
    done
    if [ "$goos" = linux ]; then
        for fact in "compositor (DALGONA_COMPOSITOR):$compositor" "portal backend (DALGONA_PORTAL_BACKEND):$portal" "desktop packages:$packages"; do
            [ -n "${fact#*:}" ] || missing="$missing; ${fact%%:*}"
        done
    fi
    evidence=()
    if [ -z "$missing" ]; then
        display_backend=$backend
        case "$goos" in darwin) display_backend=appkit ;; windows) display_backend=win32 ;; esac
        {
            printf '{\n  "commit": %s,\n' "$(json_str "$(git rev-parse HEAD)")"
            printf '  "pins": [{"module": "go", "version": %s}, {"module": "gioui.org", "version": %s}],\n' \
                "$(json_str "$(go env GOVERSION)")" "$(json_str "$(go list -m gioui.org | awk '{ print $2 }')")"
            printf '  "host": {\n    "os": %s,\n    "arch": %s,\n' "$(json_str "$goos")" "$(json_str "$(go env GOARCH)")"
            printf '    "edition": %s,\n    "build": %s,\n    "updates": %s,\n' "$(json_str "$edition")" "$(json_str "$build")" "$(printf '%s\n' "$updates" | json_list)"
            printf '    "image_digest": %s,\n    "execution": %s,\n' "$(json_str "$DALGONA_IMAGE_DIGEST")" "$(json_str "${DALGONA_EXECUTION:-native}")"
            printf '    "display": {"backend": %s, "virtual": %s},\n' "$(json_str "$display_backend")" "$(json_str "$virtual")"
            printf '    "gpu": %s,\n    "driver": %s,\n    "locale": %s,\n    "input_method": %s' "$(json_str "$gpu")" "$(json_str "$driver")" "$(json_str "$locale")" "$(json_str "$input")"
            if [ "$goos" = linux ]; then
                printf ',\n    "linux": {"kernel": %s, "compositor": %s, "desktop_packages": %s, "portal_backend": %s}' \
                    "$(json_str "$(uname -r)")" "$(json_str "$compositor")" "$(printf '%s\n' "$packages" | json_list)" "$(json_str "$portal")"
            fi
            printf '\n  },\n  "checks": [],\n  "missing": []\n}\n'
        } > "$host"
        evidence=("DALGONA_EVIDENCE_HOST=$host" "DALGONA_EVIDENCE_LOG=$record" "DALGONA_EVIDENCE_RECORD=$base.json")
    fi
    {
        echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "commit: $(git rev-parse HEAD)"
        echo "worktree changes: $(git status --porcelain | wc -l)"
        echo "system: $(uname -a)"
        if [ -r /etc/os-release ]; then echo "os: $(. /etc/os-release && echo "$PRETTY_NAME")"; fi
        echo "session: ${XDG_SESSION_TYPE:-unknown} ${XDG_CURRENT_DESKTOP:-unknown}"
        echo "backend: $backend"
        echo "go: $(go version)"
        echo "tags: $tags,guiintegration"
    } > "$record"
    status=0
    "${session[@]}" "${evidence[@]}" go test -count=1 -v -tags "$tags,guiintegration" ./cmd/dalgona-gui/... ./internal/dalgona/gui/... 2>&1 | tee -a "$record" || status=$?
    echo "result: exit $status" >> "$record"
    echo "desktop-smoke: text log in $record"
    if [ -n "$missing" ]; then
        echo "desktop-smoke: no platform evidence record; record these facts:${missing#;}" >&2
        exit 1
    fi
    echo "desktop-smoke: platform evidence record in $base.json"
    exit "$status"

desktop-fonts:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v uv >/dev/null || { echo "desktop-fonts: uv is required to run fontTools" >&2; exit 1; }
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    out=internal/dalgona/gui/platform/fontfaces
    fetch() {
        curl -fsSL -o "$work/$1" "$2"
        printf '%s  %s\n' "$3" "$work/$1" | sha256sum --check --quiet
    }
    fetch bricolage.ttf 'https://raw.githubusercontent.com/google/fonts/6ce172f74aa355ea43eb964fa4a91570a4d3064d/ofl/bricolagegrotesque/BricolageGrotesque%5Bopsz,wdth,wght%5D.ttf' 413e7357809ddd12fd80a96a8a396de0e401638d4acd3cb3e37532f0472ac682
    fetch hanken.ttf 'https://raw.githubusercontent.com/google/fonts/c1eda9233c33ad7775b27efd794f931095cf6133/ofl/hankengrotesk/HankenGrotesk%5Bwght%5D.ttf' 813b3f8fa0965405669a89b38e51bbefd95eef6b8e20d1cb2d8c10cce062662f
    fetch pretendard.zip 'https://github.com/orioncactus/pretendard/releases/download/v1.3.9/Pretendard-1.3.9.zip' 04be351a74d6bf7d60c480a3087e51d185485d35a52023142af1df19eb8c428a
    unzip -q -o -j "$work/pretendard.zip" public/static/Pretendard-Regular.otf public/static/Pretendard-SemiBold.otf public/static/Pretendard-Bold.otf -d "$work"
    cp "$work/Pretendard-Regular.otf" "$out/Pretendard-400.otf"
    cp "$work/Pretendard-SemiBold.otf" "$out/Pretendard-600.otf"
    cp "$work/Pretendard-Bold.otf" "$out/Pretendard-700.otf"
    uv run --no-project --python 3.15 --with 'fonttools[woff]==4.68.0' python - "$work/bricolage.ttf" "$work/hanken.ttf" "$out" web/src/lib/theme/symbols-nerd-subset.woff2 <<'EOF'
    import sys
    from pathlib import Path
    from fontTools.ttLib import TTFont
    from fontTools.varLib import instancer

    bricolage, hanken, out, icons = sys.argv[1], sys.argv[2], Path(sys.argv[3]), sys.argv[4]
    jobs = [
        (bricolage, "Bricolage Grotesque", "BricolageGrotesque", {"opsz": 14, "wdth": 100}, (600, 650, 700)),
        (hanken, "Hanken Grotesk", "HankenGrotesk", {}, (400, 600, 700)),
    ]
    for src, family, stem, pinned, weights in jobs:
        for weight in weights:
            static = instancer.instantiateVariableFont(TTFont(src, recalcTimestamp=False), {**pinned, "wght": weight})
            names = static["name"]
            for name_id in (1, 2, 3, 4, 6, 16, 17, 21, 22, 25):
                names.removeNames(nameID=name_id)
            names.setName(family, 1, 3, 1, 0x409)
            names.setName("Regular", 2, 3, 1, 0x409)
            names.setName(f"{stem}-{weight}", 3, 3, 1, 0x409)
            names.setName(f"{family} {weight}", 4, 3, 1, 0x409)
            names.setName(f"{stem}-{weight}", 6, 3, 1, 0x409)
            static.save(out / f"{stem}-{weight}.ttf")
    subset = TTFont(icons, recalcTimestamp=False)
    subset.flavor = None
    subset.save(out / "SymbolsNerdSubset.ttf")
    EOF
    (cd "$out" && sha256sum --check SHA256SUMS)

desktop-package:
    #!/usr/bin/env bash
    set -euo pipefail
    host=$(go env GOHOSTOS)
    target=$(go env GOOS)
    if [ "$target" != "$host" ]; then
        printf 'desktop-package: GOOS %s does not match host %s; package on the target OS\n' "$target" "$host" >&2
        exit 1
    fi
    case "$host" in
        darwin) exec bash deploy/desktop/package-macos.sh ;;
        windows) exec powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File deploy/desktop/package-windows.ps1 ;;
        linux) ;;
        *) printf 'desktop-package: no package target for %s\n' "$host" >&2; exit 1 ;;
    esac
    command -v dpkg-deb >/dev/null || { echo "desktop-package: dpkg-deb is not installed; install the dpkg package" >&2; exit 1; }
    arch=$(go env GOARCH)
    case "$arch" in
        amd64|arm64) ;;
        *) echo "desktop-package: GOARCH $arch has no .deb target; use amd64 or arm64" >&2; exit 1 ;;
    esac
    tags=$("{{just_executable()}}" --justfile "{{justfile()}}" desktop-tags)
    case ",$tags," in
        *,nox11,*|*,nowayland,*) echo "desktop-package: the package needs the X11 and Wayland backends; this host builds with tags $tags" >&2; exit 1 ;;
    esac
    name=$(git config --global user.name) && email=$(git config --global user.email) || { echo "desktop-package: set the global git identity; the Maintainer field uses it" >&2; exit 1; }
    if tag=$(git describe --tags --exact-match HEAD 2>/dev/null); then
        version=${tag#v}
    else
        version="0.0.0~git$(git log -1 --format=%cd --date=format:%Y%m%d).$(git rev-parse --short=12 HEAD)"
    fi
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)
    export SOURCE_DATE_EPOCH
    stage=$(mktemp -d)
    trap 'rm -rf "$stage"' EXIT
    root="$stage/dalgona-gui"
    install -d -m 0755 "$root/DEBIAN" "$root/usr/bin" "$root/usr/lib/dalgona-gui" "$root/usr/share/applications" "$root/usr/share/icons/hicolor/scalable/apps" "$root/usr/share/doc/dalgona-gui"
    go build -tags "$tags" -trimpath -o "$root/usr/bin/dalgona-gui" ./cmd/dalgona-gui
    CGO_ENABLED=0 go build -trimpath -o "$root/usr/lib/dalgona-gui/dalgona" ./cmd/dalgona
    install -m 0644 assets/brand/dalgona.svg "$root/usr/share/icons/hicolor/scalable/apps/dalgona.svg"
    cat LICENSE.md THIRD_PARTY_NOTICES.md > "$root/usr/share/doc/dalgona-gui/copyright"
    printf '%s\n' '[Desktop Entry]' 'Type=Application' 'Name=dalgona' 'Comment=Agent workbench' 'Exec=dalgona-gui' 'Icon=dalgona' 'Terminal=false' 'Categories=Development;' > "$root/usr/share/applications/dalgona.desktop"
    recommends=xdg-desktop-portal
    case ",$tags," in
        *,novulkan,*) ;;
        *) recommends="$recommends, libvulkan1" ;;
    esac
    printf '%s\n' "Package: dalgona-gui" "Version: $version" "Architecture: $arch" "Maintainer: $name <$email>" \
        "Depends: libc6, libegl1, libgles2, libwayland-client0, libwayland-cursor0, libwayland-egl1, libxkbcommon0, libx11-6, libx11-xcb1, libxkbcommon-x11-0, libxcursor1, libxfixes3" \
        "Recommends: $recommends" "Section: devel" "Priority: optional" "Homepage: https://github.com/gosuda/dalgona" \
        "Description: dalgona native desktop client" " The Gio desktop client for dalgona, with its compatible daemon executable." > "$root/DEBIAN/control"
    chmod -R go-w,a+rX "$root"
    find "$root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
    mkdir -p dist
    out="dist/dalgona-gui_${version}_${arch}.deb"
    dpkg-deb --root-owner-group -Zxz --build "$root" "$out"
    contents=$(dpkg-deb --contents "$out")
    for path in ./usr/bin/dalgona-gui ./usr/lib/dalgona-gui/dalgona ./usr/share/applications/dalgona.desktop ./usr/share/icons/hicolor/scalable/apps/dalgona.svg ./usr/share/doc/dalgona-gui/copyright; do
        grep -q " $path\$" <<<"$contents" || { echo "desktop-package: $out lacks $path" >&2; exit 1; }
    done
    if command -v desktop-file-validate >/dev/null; then
        desktop-file-validate "$root/usr/share/applications/dalgona.desktop"
    else
        echo "desktop-package: desktop-file-validate is not installed; the desktop entry is not validated" >&2
    fi
    echo "desktop-package: $out assembled; install, upgrade, launch and uninstall tests need a clean Ubuntu 24.04 system and did not run"

verify-go: fmt-check vet lint test vuln

# CI-only native-runner proof. fmt-check, lint and vuln are
# platform-independent and stay on the Linux lanes; the race detector stays
# on the Linux test lane. Native runners prove vet, compilation and a test
# pass on their own operating system.
verify-native-build:
    go build ./...

verify-native-test:
    go test -count=1 ./...

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
