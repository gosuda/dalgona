# Codex protocol as core

## Invariant

Clients MUST use the Codex app-server v2 protocol to reach the agent. The protocol MUST remain a client boundary, not a dependency of the core. Core packages MUST NOT import an interface or battery package.

The transport MUST carry one JSON object per line. It MUST preserve request identifiers. It MUST preserve notification order, except for merged text deltas. Under backpressure, text deltas MAY merge as [protocol.md](../spec/protocol.md) defines. A merged delta MAY then reach the client before a message that was queued between the two deltas. Unknown methods MUST return `-32601`. The server MUST ignore unknown parameter fields for forward compatibility.

The wire package `internal/dal/rpc` MUST hold only framing, wire types, codecs, and the connection. It MUST NOT import another internal package. Host-dependent method handlers MUST live in `internal/dal/rpcserver`. A client MUST import `internal/dal/rpc` and MUST NOT import `internal/dal/rpcserver`.

## Scope

This decision applies to `internal/dal/rpc`, `internal/dal/rpcserver`, and their terminal, web, and desktop consumers. [protocol.md](../spec/protocol.md) defines framing and backpressure. The vendored schemas are under `internal/dal/rpc/testdata/codex-schema`.

## Enforcement

`internal/dal/rpc/schema_test.go` and `internal/dal/rpcserver/schema_test.go` validate encoded protocol objects against the vendored Codex schemas. The `core`, `clients`, `rpc-wire`, and `tui-imports-only-rpc` depguard rules in [.golangci.yml](../../../.golangci.yml) enforce import direction. `just lint` and `just test` run these checks. A schema check MUST NOT be reported as a live Codex desktop test.

## Rejected alternatives

- One package for the wire and the handlers. It lost because every client then linked the agent, journal, and provider code through the handlers.
