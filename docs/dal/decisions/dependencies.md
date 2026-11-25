# Dependencies

## Invariants

- Wire codecs MUST NOT use reflection. JSON on the wire, in the journal, and on hot paths MUST use `encoding/json/jsontext`. Protobuf MUST use `google.golang.org/protobuf/encoding/protowire`.
- `encoding/json/v2` reflection MAY decode or encode cold data that the program reads once, such as config-shaped files.
- The module MUST NOT import a provider SDK. Each provider adapter speaks HTTP and SSE with `net/http` and `jsontext`.
- Code MUST NOT copy or allocate where a slice, a reused buffer, or a stream does the job.
- A sandbox service SDK MAY use reflection internally. No reflection-free client exists for those services.

Scope: every package in this module. Enforcement: the `fast` rule of depguard in `.golangci.yml` rejects `encoding/json` v1, the reflection protobuf packages, the provider SDKs, and the slower alternatives to each pick. The pre-commit hook in `lefthook.yml` runs `just verify`, which includes `lint` in `justfile`, before every commit. `TestDependencyTableMatchesGoMod` in `internal/pingate` fails when a direct requirement of `go.mod` or a tool has no row in `## Grounded pins`. It also fails when a module has two rows, when a row holds another version, or when a row names no required module. It fails when the `Go toolchain` row differs from the `toolchain` line, or when a second pins table exists. A step that moves a module into the direct block of `go.mod` adds its row in the same commit.

## Grounded pins

Grounded on 2026-11-11; the rows moved by the dependency upgrade of 2026-11-19 re-grounded on that date. Each pin equals the latest stable version on its release channel on its grounding date. The Go toolchain row names the newest stable release on `https://go.dev/dl/?mode=json`.

The release channel for Go modules is `https://proxy.golang.org/<module>/@latest`. The support window for Go is `https://endoflife.date/api/v1/products/go/`. Go 1.27 and Go 1.26 are supported. No end-of-life date is set.

Advisories come from the OSV batch query below, from OSV list pages `https://osv.dev/list?q=<module>&ecosystem=Go`, and from the GitHub security advisory pages of each project.

This section lists the modules that `go.mod` requires directly, and the three tools. Indirect modules follow `go mod tidy`, except the floors in `## Advisory floors`.

|Module|Pin|Released|Use|OSV batch|
|---|---|---|---|---|
|Go toolchain|go1.27.4|2026-11-10|newest supported major. CI reads it from go.mod.|none|
|charm.land/bubbletea/v2|v2.1.2|2026-11-18|TUI|none|
|charm.land/bubbles/v2|v2.2.1|2026-08-24|TUI spinner, key bindings and viewport|none|
|charm.land/glamour/v2|v2.0.1|2026-06-12|TUI markdown|none|
|charm.land/lipgloss/v2|v2.1.1|2026-11-15|TUI|none|
|codeberg.org/readeck/go-readability/v2|v2.1.3|2026-09-30|webfetch article extraction. MIT licence.|none|
|gioui.org|v0.10.3|2026-09-28|native desktop client window, input, layout, text shaping and rendering in `cmd/dalgona-gui` and `internal/dalgona/gui`, built only with the `gui` tag. Licence Unlicense OR MIT. Decision in `docs/dalgona/decisions/native-client.md`. Grounded on 2026-11-24 from `https://proxy.golang.org/gioui.org/@v/v0.10.3.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/alecthomas/chroma/v2|v2.27.0|2026-06-17|syntax highlighting of code blocks and diagram sources in `internal/dalgona/gui/document/code` and `internal/dalgona/gui/figure`. Licence MIT, per `COPYING` at v2.27.0. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/alecthomas/chroma/v2/@v/v2.27.0.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/charmbracelet/x/conpty|v0.2.0|2025-11-17|Windows pseudo-console for terminal sessions in `internal/dal/runner`. Licence MIT. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/charmbracelet/x/conpty/@v/v0.2.0.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/creack/pty|v1.1.24|2024-10-31|Unix pseudo-terminal for terminal sessions in `internal/dal/runner`. Licence MIT. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/creack/pty/@v/v1.1.24.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/go-text/typesetting|v0.3.5|2026-09-04|OpenType parsing and shaping for math and figure text in `internal/dalgona/gui/figure`. Licence Unlicense OR BSD-3-Clause. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/go-text/typesetting/@v/v0.3.5.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/yuin/goldmark|v1.8.7|2026-10-30|CommonMark parsing for transcript documents in `internal/dalgona/gui/document`. Licence MIT. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/yuin/goldmark/@v/v1.8.7.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|golang.org/x/image|v0.48.0|2026-11-09|font, vector and image decoding in `internal/dalgona/gui`. Licence BSD-3-Clause. Grounded on 2026-11-24 from `https://proxy.golang.org/golang.org/x/image/@v/v0.48.0.info`, and the OSV query for this pin on that date returned no vulnerability. v0.26.0, the version first required, had open Go advisories.|none|
|github.com/godbus/dbus/v5|v5.2.4|2026-10-13|D-Bus client for the Linux AT-SPI2 accessibility bridge and the XDG FileChooser and Settings portals in `internal/dalgona/gui/platform`. Licence BSD-2-Clause, per `LICENSE` at v5.2.4. Grounded on 2026-11-24 from `https://proxy.golang.org/github.com/godbus/dbus/v5/@v/v5.2.4.info`, and the OSV query for this pin on that date returned no vulnerability.|none|
|github.com/JohannesKaufmann/html-to-markdown/v2|v2.5.2|2026-06-07|webfetch HTML to Markdown conversion. MIT licence.|none|
|github.com/aymanbagabas/go-udiff|v0.4.2|2026-11-15|unified line diffs in `internal/dal/textdiff`. Licence: BSD-3-Clause and MIT, per `LICENSE-BSD` and `LICENSE-MIT` at v0.4.2.|none|
|github.com/charmbracelet/x/ansi|v0.11.10|2026-11-18|TUI measure, text width, and truncation, the method of the Bubble Tea renderer.|none|
|github.com/charmbracelet/colorprofile|v0.4.3|2026-03-09|TUI colour downsampling to the terminal's profile, and the `NO_COLOR` rule.|none|
|github.com/charmbracelet/x/term|v0.2.2|2025-10-31|terminal detection in TUI anchor output and in the interface entry point|none|
|github.com/charmbracelet/x/vt|v0.1.0|2026-10-12|test-only terminal emulator for the print test, MIT licence.|none|
|github.com/odvcencio/gotreesitter|v0.57.0|2026-11-08|syntax package for the symbols, definition, syntax and rewrite modes, with 23 grammars. Licence MIT, per `LICENSE` at v0.57.0. Decision in `docs/dal/decisions/syntax-package.md`.|none|
|github.com/coder/websocket|v1.8.15|2026-06-15|websocket|none|
|github.com/cockroachdb/pebble/v2|v2.1.7|2026-11-17|journal mirror, pure Go|none|
|github.com/coreequip/mermaidascii|v0.3.1|2026-11-09|mermaid fences render as text art in the TUI, a Go port of the beautiful-mermaid ASCII renderer. MIT licence.|none|
|github.com/containerd/errdefs|v1.0.0|2024-10-08|Docker runner error types|none|
|github.com/landlock-lsm/go-landlock|v0.11.2|2026-11-10|Linux write confinement|none|
|github.com/moby/moby/api|v1.57.0|2026-11-13|Docker runner API types|none|
|github.com/moby/moby/client|v0.6.3|2026-11-13|Docker runner|none|
|github.com/modal-labs/modal-client/go|v0.11.2|2026-11-16|Modal runner|none|
|github.com/pelletier/go-toml/v2|v2.4.3|2026-07-05|config|none|
|github.com/sandbox0-ai/sdk-go|v0.10.6|2026-11-15|Sandbox0 runner|none|
|github.com/santhosh-tekuri/jsonschema/v6|v6.0.3|2026-06-28|protocol schema tests only|none|
|github.com/superfly/sprites-go|v0.2.2|2026-10-20|Sprites runner|none|
|go.starlark.net|v0.0.0-20261021141710-4f4267fb140d|2026-10-21|scripts, dal eval|none|
|golang.org/x/net|v0.63.0|2026-11-17|HTML tokenizing in web fetch|none|
|golang.org/x/sync|v0.25.0|2026-10-15|errgroup and singleflight|none|
|golang.org/x/sys|v0.50.0|2026-11-06|OS calls|none|
|golang.org/x/text|v0.44.0|2026-11-09|Unicode normalization in advisor guard, and East Asian width data in the TUI test `internal/dal/tui/band_test.go`|none|
|google.golang.org/protobuf|v1.36.12|2026-08-10|encoding/protowire only|none|
|modernc.org/quickjs|v0.27.0|2026-10-24|js eval cells and nomnoml diagram rendering, pure Go QuickJS|none|
|oss.terrastruct.com/d2|v0.7.2|2026-08-07|d2 fences render as text art in the TUI via d2lib and d2ascii|none|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|none|
|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|none|
|golang.org/x/vuln|v1.10.0|2026-11-17|govulncheck tool|none|
|github.com/google/codesearch|v1.2.0|2020-03-29|search index for the grep mode, see `search-index.md`. Licence BSD-3-Clause. No module requirements.|none|

The OSV column holds the result of `POST https://api.osv.dev/v1/querybatch` for each row's exact pin, run on 2026-11-15. One batch of 51 queries covered the toolchain, every row of this table except the go-readability and html-to-markdown rows, and every pin in `web/package.json`. A supplemental batch of 2 queries on the same day covered those two pins. The toolchain query used the package `stdlib` of ecosystem Go. The response held 51 results, and none listed a vulnerability. `none` means that the result for the pin listed no vulnerability. A control query for `golang.org/x/text` v0.3.0 returned GO-2020-0015, so the query form finds a known advisory. The dependency upgrade of 2026-11-19 moved ten Go pins and eight npm pins in `web/package.json` and `deploy/cloudflare/package.json`. A batch of 18 queries on 2026-11-19 covered those exact new pins, and none listed a vulnerability. On 2026-11-24 one batch of 44 queries covered every Go module row of this table, and a query for `stdlib` 1.27.4 covered the toolchain. None listed a vulnerability. In that run, `golang.org/x/image` v0.26.0 returned open Go advisories, so the row pins v0.48.0.

The Released column of each module row equals the `Time` field of `https://proxy.golang.org/<module>/@v/<version>.info`, read on 2026-11-15; the ten rows re-grounded on 2026-11-19 read on that date, and the rows grounded on 2026-11-24 read on that date. The toolchain row equals the go1.27.4 entry of `https://go.dev/doc/devel/release`.

## Advisory floors

`go.mod` raises these indirect modules above the highest version that any dependent requires. At that lower version, `just vuln` finds a reachable advisory. `go mod tidy` keeps each raised line. An upgrade MUST NOT lower a floor. Enforcement: `vuln` in `justfile` runs govulncheck in `just verify` and in CI, and it fails when a floor falls below its fix.

|Modules|Floor|Released|Advisory|Lower version from|Exit|
|---|---|---|---|---|---|
|go.opentelemetry.io/otel, otel/metric, otel/trace|v1.47.0|2026-10-02|GO-2026-6974, fixed in otel/trace v1.47.0. `github.com/moby/moby/client` links otelhttp. govulncheck resolves the response body read in `internal/dal/model/stream.go` and the transport in `internal/dalgona/report/reporter.go` to otelhttp, which calls otel/trace.|v1.45.0, from `github.com/securego/gosec/v2` v2.29.0 under golangci-lint v2.15.0|Remove the row when a dependent requires v1.47.0 or later.|
|github.com/gorilla/websocket|v1.5.4-0.20250319132907-e064f32e3674|2025-03-19|GO-2026-6278. v1.5.3 went back to a `math/rand` mask key. Commit d67f41855da4 on `main` restores `crypto/rand`, and no tagged release holds it. The `sandbox0` and `sprites` services in `internal/dalgona/sandboxsvc` reach it through their SDKs.|v1.5.3, from `github.com/superfly/sprites-go` v0.2.2 and gosec v2.29.0. `github.com/sandbox0-ai/sdk-go` v0.10.6 requires v1.5.1. Both SDK pins are their latest release.|Move to the first tag that holds the fix. Remove the row when every dependent requires a fixed version.|

otel v1.47.0 adds otel/log v1.47.0 and keeps `go.opentelemetry.io/auto/sdk` at v1.2.1. otelhttp stays at v0.70.0, the version gosec requires. otel/sdk stays at v1.45.0 in the module graph, and no package that this module builds imports it. The gorilla/websocket floor is the head of `main`, the latest commit on the only channel that holds the fix.

The Released column equals the `Time` field of `https://proxy.golang.org/<module>/@v/<version>.info` for each floor, read on 2026-11-24. One OSV batch of 8 queries ran on 2026-11-24. Six queries covered the floors, otel/log v1.47.0, and otelhttp v0.70.0, and none listed a vulnerability. Two control queries for gorilla/websocket v1.5.3 and otel/trace v1.45.0 returned GO-2026-6278 and GO-2026-6974.

## Runtime endpoints

|Endpoint|Purpose|Cache|Fallback|Data sent|
|---|---|---|---|---|
|`registry.npmjs.org/@openai%2Fcodex/latest`|latest released Codex client version for the `client_version` query of the ChatGPT model list|`codex-client-version.json` in the cache directory, 4 hours|a valid cached version, else `Models` fails with `chatgpt: client version is unavailable: <cause>`|the client address only|

## Rejected alternatives

|Choice|Replacement|Reason|
|---|---|---|
|github.com/openai/openai-go/v3|`net/http` + `jsontext`|reflection codec|
|github.com/anthropics/anthropic-sdk-go/v2|`net/http` + `jsontext`|reflection codec|
|google.golang.org/genai|`net/http` + `jsontext`|reflection codec|
