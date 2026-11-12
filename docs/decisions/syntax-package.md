# Syntax package

## Invariant

Only `internal/syntax` MUST import `github.com/odvcencio/gotreesitter`. Callers MUST see symbols, captures and reports, never trees, nodes or queries of the library. The package MUST import no grammar aggregate and no copyleft grammar. Parsing MUST run under a time limit, a memory budget and work limits, and every tree MUST be released after its rows are read.

## Scope

This decision applies to `internal/syntax`, its embedded queries under `internal/syntax/queries`, and the import rules `syntax` and `copyleft` in `.golangci.yml`. The search tool and the rewrite tool use the package through its exported API only. Each search mode that uses the package is specified in [search.md](../spec/search.md) by the step that adds that mode.

## Enforcement

`just lint` runs the `syntax` rule, which denies the library outside `internal/syntax`, and the `copyleft` rule, which denies the aggregate package and the copyleft grammars everywhere. `internal/syntax/syntax_test.go` checks the kind table, the extension table, the Go fixture rows, context cancellation and the stopped parse.

## Measurements

Baseline linux/amd64 binaries before the package existed, built with go1.27.4 from commit 30f8f047:

|Binary|Bytes|
|---|---|
|`dal`|36598376|
|`dalgona`|69772184|

Nothing in `cmd/` imports `internal/syntax` at this step, so these sizes hold until the search tool imports it. The sizes after the grammars are linked are recorded by Code symbols step 9.

## Rejected alternatives

- A wrapper that returns trees and nodes. It lost because each caller would then repeat the parser setup, the time limit and the release rule, and the library import would spread across packages.
- The library's aggregate grammar package. It lost because it embeds the four copyleft grammars (caddy, disassembly, jq under GPL-3.0 and nim under MPL-2.0) in every build, per `licenses/grammars.json`.
- The library's inferred tags queries. They lost because they name kinds this package does not choose and they cover only some languages, so the closed kind set would not hold.
