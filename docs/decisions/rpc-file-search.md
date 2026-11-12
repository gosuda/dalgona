# RPC file search

## Invariant

The `fuzzyFileSearch` method MUST list files through `search.Files`, the walker that the `search` tool uses. `internal/rpc` MUST NOT walk a workspace tree or apply ignore rules of its own.

## Scope

This decision applies to the import of `internal/tool/search` by `internal/rpc`, and to `internal/rpc/serve_fuzzy.go`. The scoring rule of the method is in [Fuzzy file search](../spec/search.md#fuzzy-file-search).

## Enforcement

`TestFuzzyFileSearchRanksAndHonoursGitignore` checks that an ignored file is not listed. `TestFuzzyFileSearchReusesWalk` checks that two requests in one window walk a root once. `TestFuzzyScoreRule` checks the score of three cases by hand.

## Rejected alternatives

- A second walker in `internal/rpc`. It lost because its ignore rules would drift from the `search` tool, so a file that the model cannot search would appear in the client's list.
