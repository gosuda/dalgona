# RPC thread search

## Invariant

The `dalgona/thread/search` method MUST query the journal mirror through `Host.SearchThreads`. `internal/rpc` MUST NOT build search SQL or read `state.db` itself. The query and every snippet MUST pass the host masker.

## Scope

This decision applies to the import of `internal/journal` by `internal/rpc`, to `internal/rpc/serve_search.go`, and to the FTS5 index rules in [journal.md](../spec/journal.md). The wire contract is in [protocol.md](../spec/protocol.md#thread-search).

## Enforcement

`TestSearchMatchesBruteForce` compares the index with an independent scan of `Replay`. `TestSearchMasksQueryAndSnippet` checks the query and snippet masking. `TestThreadSearchValidation` checks the parameter errors of the method.

## Rejected alternatives

- A search SQL string in `internal/rpc`. It lost because the index schema belongs to the journal mirror, so a migration would break the method silently.
- An unmasked snippet. It lost because a secret stored after the text was journaled would surface through search.
