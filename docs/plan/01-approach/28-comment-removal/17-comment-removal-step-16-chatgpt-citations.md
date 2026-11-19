#### Comment removal step 16. ChatGPT citations

Anchor: Milton Erickson

- Files: `internal/model/chatgpt/chatgpt.go`, `docs/spec/providers.md`.
- Change: delete the comment lines of rows 67 and 68 (`chatgpt.go:15` and `chatgpt.go:46`).
- Spec text, `docs/spec/providers.md`, under `## Subscription and IDE providers`: as a new paragraph after the paragraph that starts `The `chatgpt` provider MUST send`: The default `chatgpt` base URL MUST be `pi`'s `DEFAULT_CODEX_BASE_URL` with `/codex` appended. The static Codex model list MUST come from `codex-rs/models-manager/models.json` in the Codex repository.
- Tests that fail before the change: none. The base URL is pinned by the `DefaultBaseURL` constant (`internal/model/chatgpt/chatgpt.go:17`).
- Check: `just verify` passes.
- Lands after: Comment removal step 15

