#### Comment removal step 11. Antigravity citations

Anchor: Otl Aicher

- Files: `internal/model/antigravity/antigravity.go`, `internal/model/antigravity/models.go`, `internal/model/antigravity/project.go`, `docs/spec/providers.md`.
- Change: delete the comment lines of rows 54 to 57.
- Spec text, `docs/spec/providers.md`: as a new paragraph under `## Subscription and IDE providers`, after the paragraph that starts `The `antigravity` provider MUST send`: The Antigravity client headers MUST follow oh-my-pi's `packages/catalog/src/wire/gemini-headers.ts`, with the offline fallback version `2.26.0`. The `requestId` form MUST follow `buildAntigravityRequestEnvelope` in oh-my-pi's `packages/ai/src/providers/google-gemini-cli.ts`. The static Antigravity list MUST name the `google-antigravity` models with thinking mode `google-level` in oh-my-pi's `packages/catalog/src/models.json`. The onboarding tier and timeout MUST follow oh-my-pi's `packages/ai/src/registry/oauth/google-antigravity.ts`.
- Tests that fail before the change: none. Behaviour does not change.
- Check: `just verify` passes.
- Lands after: Comment removal step 10

