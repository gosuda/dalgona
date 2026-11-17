# RPC file search

## Invariant

The `fuzzyFileSearch` method MUST list files through `search.Files`, the walker that the `search` tool uses. `internal/dal/rpc` MUST NOT walk a workspace tree or apply ignore rules of its own.

## Scope

This decision applies to the import of `internal/dal/tool/search` by `internal/dal/rpc`, and to `internal/dal/rpc/serve_fuzzy.go`. The scoring rule of the method is in [Fuzzy file search](../spec/search.md#fuzzy-file-search).

## Enforcement

`TestFuzzyFileSearchRanksAndHonoursGitignore` checks that an ignored file is not listed. `TestFuzzyFileSearchReusesWalk` checks that two requests in one window walk a root once. `TestFuzzyScoreRule` checks the score and the match positions of each case by hand. `TestFuzzyFileSearchOrdersByTheGreedyMatch` checks the result order against the greedy rule.

## Rejected alternatives

- A second walker in `internal/dal/rpc`. It lost because its ignore rules would drift from the `search` tool, so a file that the model cannot search would appear in the client's list.
