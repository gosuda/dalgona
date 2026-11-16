# Dependencies

## Invariants

- Wire codecs MUST NOT use reflection. JSON on the wire, in the journal, and on hot paths MUST use `encoding/json/jsontext`. Protobuf MUST use `google.golang.org/protobuf/encoding/protowire`.
- `encoding/json/v2` reflection MAY decode or encode cold data that the program reads once, such as config-shaped files.
- The module MUST NOT import a provider SDK. Each provider adapter speaks HTTP and SSE with `net/http` and `jsontext`.
- Code MUST NOT copy or allocate where a slice, a reused buffer, or a stream does the job.
- A sandbox service SDK MAY use reflection internally. No reflection-free client exists for those services.

Scope: every package in this module. Enforcement: the `fast` rule of depguard in `.golangci.yml` rejects `encoding/json` v1, the reflection protobuf packages, the provider SDKs, and the slower alternatives to each pick. The pre-commit hook in `lefthook.yml` runs `just verify`, which includes `lint` in `justfile`, before every commit. `TestDependencyTableMatchesGoMod` in `internal/pingate` fails when a direct requirement of `go.mod` or a tool has no row in `## Grounded pins`. It also fails when a module has two rows, when a row holds another version, or when a row names no required module. It fails when the `Go toolchain` row differs from the `toolchain` line, or when a second pins table exists. A step that moves a module into the direct block of `go.mod` adds its row in the same commit.

## Grounded pins

Grounded on 2026-11-11. Each pin equals the latest stable version on its release channel on that date. The Go toolchain row names the newest stable release on `https://go.dev/dl/?mode=json`.

The release channel for Go modules is `https://proxy.golang.org/<module>/@latest`. The support window for Go is `https://endoflife.date/api/v1/products/go/`. Go 1.27 and Go 1.26 are supported. No end-of-life date is set.

Advisories come from the OSV batch query below, from OSV list pages `https://osv.dev/list?q=<module>&ecosystem=Go`, and from the GitHub security advisory pages of each project.

This section lists the modules that `go.mod` requires directly, and the three tools. Indirect modules follow `go mod tidy`.

|Module|Pin|Released|Use|OSV batch 2026-11-15|
|---|---|---|---|---|
|Go toolchain|go1.27.4|2026-11-10|newest supported major. CI reads it from go.mod.|none|
|charm.land/bubbletea/v2|v2.1.1|2026-10-28|TUI|none|
|charm.land/bubbles/v2|v2.2.1|2026-08-24|TUI spinner, key bindings and viewport|none|
|charm.land/glamour/v2|v2.0.1|2026-06-12|TUI markdown|none|
|charm.land/lipgloss/v2|v2.1.0|2026-11-09|TUI|none|
|codeberg.org/readeck/go-readability/v2|v2.1.3|2026-09-30|webfetch article extraction. MIT licence.|none|
|github.com/JohannesKaufmann/html-to-markdown/v2|v2.5.2|2026-06-07|webfetch HTML to Markdown conversion. MIT licence.|none|
|github.com/aymanbagabas/go-udiff|v0.4.1|2026-03-08|unified line diffs in `internal/textdiff`. Licence: BSD-3-Clause and MIT, per `LICENSE-BSD` and `LICENSE-MIT` at v0.4.1.|none|
|github.com/charmbracelet/x/ansi|v0.11.9|2026-10-08|TUI measure, text width, and truncation, the method of the Bubble Tea renderer.|none|
|github.com/charmbracelet/colorprofile|v0.4.3|2026-03-09|TUI colour downsampling to the terminal's profile, and the `NO_COLOR` rule.|none|
|github.com/charmbracelet/x/term|v0.2.2|2025-10-31|terminal detection in TUI anchor output|none|
|github.com/charmbracelet/x/vt|v0.1.0|2026-10-12|test-only terminal emulator for the print test, MIT licence.|none|
|github.com/odvcencio/gotreesitter|v0.57.0|2026-11-08|syntax package for the symbols, definition, syntax and rewrite modes, with 23 grammars. Licence MIT, per `LICENSE` at v0.57.0. Decision in `docs/decisions/syntax-package.md`.|none|
|github.com/coder/websocket|v1.8.15|2026-06-15|websocket|none|
|github.com/coreequip/mermaidascii|v0.3.1|2026-11-09|mermaid fences render as text art in the TUI, a Go port of the beautiful-mermaid ASCII renderer. MIT licence.|none|
|github.com/containerd/errdefs|v1.0.0|2024-10-08|Docker runner error types|none|
|github.com/landlock-lsm/go-landlock|v0.11.2|2026-11-10|Linux write confinement|none|
|github.com/moby/moby/api|v1.56.1|2026-10-01|Docker runner API types|none|
|github.com/moby/moby/client|v0.6.2|2026-10-08|Docker runner|none|
|github.com/modal-labs/modal-client/go|v0.11.1|2026-10-16|Modal runner|none|
|github.com/pelletier/go-toml/v2|v2.4.3|2026-07-05|config|none|
|github.com/sandbox0-ai/sdk-go|v0.10.5|2026-10-30|Sandbox0 runner|none|
|github.com/santhosh-tekuri/jsonschema/v6|v6.0.3|2026-06-28|protocol schema tests only|none|
|github.com/superfly/sprites-go|v0.2.2|2026-10-20|Sprites runner|none|
|go.starlark.net|v0.0.0-20261021141710-4f4267fb140d|2026-10-21|scripts, dal eval|none|
|golang.org/x/net|v0.62.0|2026-10-27|HTML tokenizing in web fetch|none|
|golang.org/x/sync|v0.25.0|2026-10-15|errgroup and singleflight|none|
|golang.org/x/sys|v0.50.0|2026-11-06|OS calls|none|
|golang.org/x/text|v0.44.0|2026-11-09|Unicode normalization in advisor guard, alongside TUI normalization and width use in `internal/tui/input.go`, `width.go`, and `band_test.go`|none|
|google.golang.org/protobuf|v1.36.12|2026-08-10|encoding/protowire only|none|
|modernc.org/quickjs|v0.27.0|2026-10-24|js eval cells and nomnoml diagram rendering, pure Go QuickJS|none|
|modernc.org/sqlite|v1.62.0|2026-11-07|journal mirror, pure Go|none|
|oss.terrastruct.com/d2|v0.7.2|2026-08-07|d2 fences render as text art in the TUI via d2lib and d2ascii|none|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|none|
|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|none|
|golang.org/x/vuln|v1.9.0|2026-10-11|govulncheck tool|none|
|github.com/google/codesearch|v1.2.0|2020-03-29|search index for the grep mode, see `search-index.md`. Licence BSD-3-Clause. No module requirements.|none|

The OSV column holds the result of `POST https://api.osv.dev/v1/querybatch` for each row's exact pin, run on 2026-11-15. One batch of 51 queries covered the toolchain, every row of this table except the go-readability and html-to-markdown rows, and every pin in `web/package.json`. A supplemental batch of 2 queries on the same day covered those two pins. The toolchain query used the package `stdlib` of ecosystem Go. The response held 51 results, and none listed a vulnerability. `none` means that the result for the pin listed no vulnerability. A control query for `golang.org/x/text` v0.3.0 returned GO-2020-0015, so the query form finds a known advisory.

The Released column of each module row equals the `Time` field of `https://proxy.golang.org/<module>/@v/<version>.info`, read on 2026-11-15. The toolchain row equals the go1.27.4 entry of `https://go.dev/doc/devel/release`.

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
