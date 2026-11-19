#### Comment removal step 6. Auth package citations

Anchor: Claude Shannon

- Files: `internal/auth/auth.go`, `internal/auth/oauth/oauth.go`, `docs/spec/daemon.md`, `docs/spec/oauth.md`.
- Change: delete the comment lines of rows 40 to 45. Each row adds the spec text below.
- Spec text, `docs/spec/daemon.md`: as a new paragraph after the paragraph that starts `dalgona login [provider]`: A conditional credential write MUST reload `auth.json` under the lock `<home>/auth.lock`, write only on a match, and report whether it wrote.
- Spec text, `docs/spec/oauth.md`: as a new paragraph directly under `# OAuth flows`, before the paragraph that starts `The OAuth package MUST`: The `chatgpt` entry MUST take its client values from Codex `codex-rs/login/src/server.rs`. The `anthropic` entry MUST take its client values from `pi`'s `packages/ai/src/auth/oauth/anthropic.ts`. The `antigravity` entry MUST take its values from `oh-my-pi`'s `packages/catalog/src/compat/rules/auth/google-antigravity.kdl`. The `devin` entry MUST take its values from `oh-my-pi`'s `packages/catalog/src/compat/rules/auth/devin.kdl`. The `cursor` entry MUST take its values from `oh-my-pi`'s `packages/ai/src/registry/oauth/cursor.ts`.
- Tests that fail before the change: none. The compare-and-report rule is pinned by `TestStore_SetCredentialIfUnchanged` (`internal/auth/cas_test.go:17`).
- Check: `just verify` passes. `grep -n "auth.lock" docs/spec/daemon.md` returns the new sentence.
- Lands after: Comment removal step 5

