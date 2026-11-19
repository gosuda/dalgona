#### Comment removal step 15. Chat-compatible model list citation

Anchor: J.R.R. Tolkien

- Files: `internal/model/chatcompat/chatcompat.go`, `docs/spec/providers.md`.
- Change: delete the comment line of row 66 (`internal/model/chatcompat/chatcompat.go:276`).
- Spec text, `docs/spec/providers.md`, under `## Capabilities and roles`: as a new paragraph after the paragraph added by Step 14: The chat-compatible model list MUST decode the response shape of `pi`'s `packages/ai/src/providers/openrouter-api.ts`.
- Tests that fail before the change: none.
- Check: `just verify` passes.
- Lands after: Comment removal step 14

