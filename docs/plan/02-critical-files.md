## Critical files

Current source paths use the `internal/dal` and `internal/dalgona` split.
The native packages listed as planned do not exist merely because this plan names them.

|Path or contract|Implementation responsibility|
|---|---|
|`internal/dal/app/cli.go` and `internal/dalgona/daemon`|Existing entry points and daemon startup. Extract shared lifecycle without copying another launch path.|
|`internal/dal/rpc/conn.go`|Wire framing, dispatch, cancellation, and the typed question response lifecycle.|
|`internal/dal/rpc/serve_track.go` and server handlers|Current event projection and protocol behavior. Add negotiated ordering, snapshot barriers, and bounded native projections.|
|`internal/dal/rpcserver` (planned)|Host-dependent handlers after the client/server package split. Preserve shared codecs and schema validation.|
|`internal/dalgona/client` (planned)|Connection ownership, endpoint identity, workspace interests, and lifecycle shared by actual callers.|
|`cmd/dalgona-gui` and `internal/dalgona/gui` (planned)|Complete Gio application, native documents, figures, terminal views, platform adapters, and packaging.|
|`internal/dal/tui`|Terminal editor, safe display projections, width mappings, tagged paste chips, and question correction.|
|`web/src/lib/session.svelte.ts` and `web/src/lib/components`|Browser state, input adapters, search eligibility, preserved drafts, and pending request cards.|
|`internal/dal/agent/entry.go` and `internal/dal/agent/tools.go`|Validation before tool preparation and effects, with existing masking and journal authority.|
|`internal/dal/tool/ask`|Question eligibility, scalar quotas, exact offered values, answer validation, and result formatting.|
|`internal/dal/journal/search.go`|The conversation-search term rule that all client eligibility checks must match.|
|`internal/dalgona/mcp`, discovery, and `internal/dal/rpc/serve_mcp.go`|MCP provenance, effective configuration, inventory versions, and runtime status.|
|[Input text](../dal/spec/input-text.md) and [cases](../dal/spec/input-text-cases.json)|Shared Unicode authority and reviewable conformance vectors.|
|[Native specifications](../dalgona/spec/native.md)|Complete component, protocol, design, document, platform, and verification requirements.|
|[Core protocol](../dal/spec/protocol.md), [journal](../dal/spec/journal.md), and [recovery](../dal/spec/recovery.md)|Existing wire and durability boundaries that the native implementation must preserve.|

Older workstreams retain their own code-tool, edit-review, hashline, and comment-gate file maps.
Read their actual current packages before applying historical line references.

