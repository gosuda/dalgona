# Dependencies

## Invariants

- Wire codecs MUST NOT use reflection. JSON on the wire, in the journal, and on hot paths MUST use `encoding/json/jsontext`. Protobuf MUST use `google.golang.org/protobuf/encoding/protowire`.
- `encoding/json/v2` reflection MAY decode or encode cold data that the program reads once, such as config-shaped files.
- The module MUST NOT import a provider SDK. Each provider adapter speaks HTTP and SSE with `net/http` and `jsontext`.
- Code MUST NOT copy or allocate where a slice, a reused buffer, or a stream does the job.
- A sandbox service SDK MAY use reflection internally. No reflection-free client exists for those services.

Scope: every package in this module. Enforcement: the `fast` rule of depguard in `.golangci.yml` rejects `encoding/json` v1, the reflection protobuf packages, the provider SDKs, and the slower alternatives to each pick. `just lint` runs it before every commit.

## Grounded pins

Grounded on 2026-11-11. Each pin equals the latest stable version on its release channel on that date. The Go toolchain row names the newest stable release on `https://go.dev/dl/?mode=json`.

The release channel for Go modules is `https://proxy.golang.org/<module>/@latest`. The support window for Go is `https://endoflife.date/api/v1/products/go/`. Go 1.27 and Go 1.26 are supported. No end-of-life date is set.

Advisories come from OSV list pages, `https://osv.dev/list?q=<module>&ecosystem=Go`, and from the GitHub security advisory pages of each project. No OSV record that the grounding checked lists a pin in this table as affected.

This section lists the modules that `go.mod` requires directly, and the three tools. Indirect modules follow `go mod tidy`.

|Module|Pin|Released|Use|OSV batch 2026-11-12|
|---|---|---|---|---|
|Go toolchain|go1.27.4|2026-11-10|newest supported major; CI reads it from go.mod|none|
|charm.land/bubbletea/v2|v2.1.1|2026-10-28|TUI|none|
|charm.land/glamour/v2|v2.0.1|2026-06-12|TUI markdown|none|
|charm.land/lipgloss/v2|v2.1.0|2026-11-09|TUI|none|
|github.com/coder/websocket|v1.8.15|2026-06-15|websocket|none|
|github.com/containerd/errdefs|v1.0.0|2024-10-08|Docker runner error types|none|
|github.com/landlock-lsm/go-landlock|v0.11.2|2026-11-10|Linux write confinement|none|
|github.com/moby/moby/api|v1.56.1|2026-10-01|Docker runner API types|none|
|github.com/moby/moby/client|v0.6.2|2026-10-08|Docker runner|none|
|github.com/modal-labs/modal-client/go|v0.11.1|2026-10-16|Modal runner|none|
|github.com/pelletier/go-toml/v2|v2.4.3|2026-07-05|config|none|
|github.com/sandbox0-ai/sdk-go|v0.10.5|2026-10-30|Sandbox0 runner|none|
|github.com/santhosh-tekuri/jsonschema/v6|v6.0.3|2026-06-28|protocol schema tests only|none|
|github.com/superfly/sprites-go|v0.2.2|2026-10-20|Sprites runner|none|
|go.starlark.net|v0.0.0-20261021141710-4f4267fb140d|2026-10-21|scripts|none|
|golang.org/x/net|v0.62.0|2026-10-27|HTML tokenizing in web fetch|none|
|golang.org/x/sync|v0.25.0|2026-10-15|errgroup and singleflight|none|
|golang.org/x/sys|v0.50.0|2026-11-06|OS calls|none|
|golang.org/x/text|v0.44.0|2026-11-09|Unicode normalization in advisor guard|none|
|google.golang.org/protobuf|v1.36.12|2026-08-10|encoding/protowire only|none|
|modernc.org/sqlite|v1.62.0|2026-11-07|journal mirror, pure Go|none|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|none|
|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|none|
|golang.org/x/vuln|v1.9.0|2026-10-11|govulncheck tool|none|

The OSV column holds the result of `POST https://api.osv.dev/v1/querybatch` for each row's exact pin, run on 2026-11-12. `none` means the response listed no vulnerability for that pin. The batch run also covered `github.com/aymanbagabas/go-udiff` v0.4.1, which a later phase adds, and it returned `none`.

## Dropped

|Choice|Replacement|Reason|
|---|---|---|
|github.com/openai/openai-go/v3|`net/http` + `jsontext`|reflection codec|
|github.com/anthropics/anthropic-sdk-go/v2|`net/http` + `jsontext`|reflection codec|
|google.golang.org/genai|`net/http` + `jsontext`|reflection codec|
