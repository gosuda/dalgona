# Dependencies

## Invariants

- Wire codecs MUST NOT use reflection. JSON on the wire, in the journal, and on hot paths MUST use `encoding/json/jsontext`. Protobuf MUST use `google.golang.org/protobuf/encoding/protowire`.
- `encoding/json/v2` reflection MAY decode or encode cold data that the program reads once, such as config-shaped files.
- The module MUST NOT import a provider SDK. Each provider adapter speaks HTTP and SSE with `net/http` and `jsontext`.
- Code MUST NOT copy or allocate where a slice, a reused buffer, or a stream does the job.
- A sandbox service SDK MAY use reflection internally. No reflection-free client exists for those services.

Scope: every package in this module. Enforcement: the `fast` rule of depguard in `.golangci.yml` rejects `encoding/json` v1, the reflection protobuf packages, the provider SDKs, and the slower alternatives to each pick. `just lint` runs it before every commit.

## Grounded pins

Grounded on 2026-11-08. Release channel: `https://proxy.golang.org/<module>/@latest`, queried on that date. Each pin below equals the latest version on that channel. Support window for Go: `https://endoflife.date/api/v1/products/go/` (1.27 and 1.26 supported, no EOL date set). Advisories: OSV `POST https://api.osv.dev/v1/querybatch` for every exact pin returned no vulnerabilities.

|Module|Pin|Released|Use|
|---|---|---|---|
|Go toolchain|go1.27.3|2026-10-19|newest supported major|
|go.starlark.net|v0.0.0-20261021141710-4f4267fb140d|2026-10-21|scripts|
|modernc.org/sqlite|v1.62.0|2026-11-07|journal mirror, pure Go|
|github.com/pelletier/go-toml/v2|v2.4.3|2026-07-05|config|
|charm.land/bubbletea/v2|v2.1.1|2026-10-28|TUI|
|charm.land/lipgloss/v2|v2.0.6|2026-08-11|TUI|
|charm.land/bubbles/v2|v2.2.1|2026-08-24|TUI|
|charm.land/glamour/v2|v2.0.1|2026-06-12|TUI markdown|
|github.com/coder/websocket|v1.8.15|2026-06-15|websocket|
|google.golang.org/protobuf|v1.36.12|2026-08-10|`encoding/protowire` only|
|github.com/modelcontextprotocol/go-sdk|v1.8.0|2026-09-04|MCP client|
|github.com/landlock-lsm/go-landlock|v0.11.0|2026-10-23|Linux write confinement|
|golang.org/x/sys|v0.49.0|2026-10-06|OS calls|
|github.com/moby/moby/client|v0.6.2|2026-10-08|Docker runner|
|github.com/modal-labs/modal-client/go|v0.11.1|2026-10-16|Modal runner|
|github.com/superfly/sprites-go|v0.2.2|2026-10-20|Sprites runner|
|github.com/sandbox0-ai/sdk-go|v0.10.5|2026-10-30|Sandbox0 runner|
|github.com/santhosh-tekuri/jsonschema/v6|v6.0.3|2026-06-28|protocol schema tests only|
|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|
|golang.org/x/vuln|v1.9.0|2026-10-11|govulncheck tool|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|

## Dropped

|Choice|Replacement|Reason|
|---|---|---|
|github.com/openai/openai-go/v3|`net/http` + `jsontext`|reflection codec|
|github.com/anthropics/anthropic-sdk-go/v2|`net/http` + `jsontext`|reflection codec|
|google.golang.org/genai|`net/http` + `jsontext`|reflection codec|
