# Codex protocol as core

## Invariant

Clients MUST use the Codex app-server v2 protocol to reach the agent. The protocol MUST remain a client boundary, not a dependency of the core. Core packages MUST NOT import an interface or battery package.

The transport MUST carry one JSON object per line. It MUST preserve request identifiers. It MUST preserve notification order, except for merged text deltas. Under backpressure, text deltas MAY merge as [protocol.md](../spec/protocol.md) defines. A merged delta MAY then reach the client before a message that was queued between the two deltas. Unknown methods MUST return `-32601`. The server MUST ignore unknown parameter fields for forward compatibility.

## Scope

This decision applies to `internal/rpc` and its terminal, web, and desktop consumers. [protocol.md](../spec/protocol.md) defines framing and backpressure. The vendored schemas are under `internal/rpc/testdata/codex-schema`.

## Enforcement

`internal/rpc/schema_test.go` validates encoded protocol objects against the vendored Codex schemas. The `core` and `clients` depguard rules in [.golangci.yml](../../.golangci.yml) enforce import direction. `just lint` and `just test` run these checks. A schema check MUST NOT be reported as a live Codex desktop test.
