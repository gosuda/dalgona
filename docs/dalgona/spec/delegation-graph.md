# Delegation graph value

A `Graph` is strict JSON with one `nodes` array. `Parse` MUST decode raw JSON and reject duplicate and unknown members. `Validate` MUST check every bound below and report shape errors first. Parsing, traversal, and saved-file loading stay outside the core value.

```go
type Graph struct {
    Nodes []Node `json:"nodes"`
}

type Node struct {
    ID      string       `json:"id"`
    Needs   []string     `json:"needs"`
    Profile string       `json:"profile"`
    Prompt  string       `json:"prompt"`
    Writes  []string     `json:"writes"`
    Schema  jsontext.Value `json:"schema,omitzero"`
}

func Parse(raw jsontext.Value) (Graph, error)
func Validate(value Graph) error
```

## Node grammar

A graph MUST contain 1 to 64 nodes, including extensions. Its serialized size MUST NOT exceed 1048576 bytes. Submitted node order is preserved. Canonical needs and writes are lexical.

Node ids MUST match `[a-z][a-z0-9_-]{0,47}`. Profile names use the same grammar. An empty profile means explorer. Each node MUST have at most 63 distinct needs. A need MUST NOT name the node itself. A prompt MUST be valid UTF-8, contain non-whitespace text, and fit within 65536 bytes.

## Writes and isolation

Each node MUST have at most 64 distinct write claims. Each claim MUST be at most 1024 UTF-8 bytes. A claim is a literal workspace-relative path or the whole-workspace value `.`. No glob syntax is interpreted.

Claims MUST reject absolute paths, backslashes, NUL, control characters, drive prefixes, empty components, dot and traversal components, and case-insensitive `.git` components. The sole dot exception is the whole-workspace value. Claims cover the named path and descendants. Existing ancestors resolve against the canonical workspace at admission and merge. Symlink escape MUST fail. Missing leaf paths are valid.

Empty writes mean no writes. An empty-writes node MUST force a read-only authority intersection with enforced read-only confinement. A writing profile MUST NOT override it. Nonempty writes require isolated execution. Actual patch paths, including rename sources and destinations, MUST remain inside the claims.

An omitted schema accepts any valid JSON. An explicit null schema is invalid. Schemas consume the shared 16384-byte, depth-16 keyword subset with no references and no external resolution.

## Ordering and overlap

Needs define execution order. Writes define merge order. Overlapping write claims between nodes MUST be ordered explicitly. Unordered overlap MUST fail validation. Patch paths outside a node's claims MUST fail with a patch-outside-claims error. Report shape errors first, then node-local errors in submission order, then unknown needs, cycles, and overlapping writes. Overlapping pairs select submission order with their lexical first overlapping path.

## Limits and errors

| Condition | Error |
|---|---|
| Node count outside bounds | `graph: nodes must contain 1 to 64 nodes` |
| Serialized size | `graph: value exceeds 1048576 bytes` |
| Invalid JSON or shape | `graph: invalid value` |
| Duplicate member | `graph: duplicate member "<name>"` |
| Unknown member | `graph: unknown member "<name>"` |
| Invalid id | `graph: invalid node id "<id>"` |
| Duplicate id | `graph: duplicate node id "<id>"` |
| Invalid profile | `graph: invalid profile "<profile>"` |
| Empty prompt | `graph: node "<id>" needs a prompt` |
| Invalid or oversized prompt | `graph: node "<id>" prompt must be valid utf-8 within 65536 bytes` |
| Too many needs | `graph: node "<id>" exceeds 63 needs` |
| Duplicate need | `graph: node "<id>" repeats need "<need>"` |
| Unknown need | `graph: node "<id>" has unknown need "<need>"` |
| Self-need | `graph: node "<id>" needs itself` |
| Cycle | `graph: dependency cycle` |
| Too many writes | `graph: node "<id>" exceeds 64 writes` |
| Invalid or escaping write | `graph: node "<id>" has invalid write path "<path>"` |
| Duplicate write | `graph: node "<id>" repeats write path "<path>"` |
| Unordered overlap | `graph: unordered writes overlap between "<a>" and "<b>" at "<path>"` |
| Patch outside claims | `graph: node "<id>" wrote outside its claims at "<path>"` |

Schema errors retain the shared schema exact strings.
