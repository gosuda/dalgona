#### Protocol step 14. `review/start`

- New `internal/rpc/serve_review.go`: `(*server).reviewStart`; `func reviewPrompt(target reviewTarget) (string, error)` with the four texts in Schemas; `type reviewTarget struct { Kind, Branch, SHA, Title, Instructions string }` where `Kind` is `uncommittedChanges`, `baseBranch`, `commit` or `custom`; `delivery` other than null or `inline` gives `unsupported delivery <d>`; `branch` MUST match `^[A-Za-z0-9._/][A-Za-z0-9._/-]*$` (`invalid branch <b>`); `sha` MUST match `^[0-9a-fA-F]{4,64}$` (`invalid sha <s>`); empty custom instructions give `instructions must not be empty`. Submit like `turnStart` (`serve_turn.go:133-163`) with `Kind: InputPrompt`; result `{reviewThreadId, turn}`, where `reviewThreadId` is the request's `threadId`.
- `responseSchemas["review/start"] = "v2/ReviewStartResponse.json"`; `schema_test.go` adds `v2/ReviewStartParams.json`.
- Test `internal/rpc` `TestReviewStartRunsAReviewTurn` (captured model body contains the uncommitted-changes prompt; bad branch `-x` and `detached` fail exactly). Fails before: `-32601`.
- Docs: `protocol.md`.

Anchor: Richard Feynman.

