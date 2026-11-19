#### Specifications step 32. Correct `docs/spec/secrets.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/secrets.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 11 | If `auth.json` cannot be reread, `Mask` MUST return `secret masking failed: auth.json could not be reread`. | internal/auth/mask.go:41 `maskReloadFailure = "secret masking failed: auth.json could not be reread"`; mask.go:61-63 `if err := s.reloadAuthIfChanged(); err != nil { s.maskErr = fmt.Errorf("reload credentials: %w", err); |
| 15 | If a placeholder with masker grammar has no known value, `UnmaskStrict` MUST return an error that names the placeholder. | internal/auth/mask.go:95 `func (s *Store) UnmaskStrict`; mask.go:105-106 `if token, ok := unresolvedPlaceholder(out); ok {` / `return "", fmt.Errorf("auth: secret placeholder %q cannot be restored: secrets.json is missin |

Use one word for one thing.

- Keep `pattern match`. Replace pattern hits at lines 11.

Structure:

- The line 11 writer rules (auth.lock, reload, merge, allocate, refresh) were not traced in this run. The split keeps every fact as written.
- Line 3 keeps its KEY=<redacted> literal unchanged.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 11 | Writers MUST serialize on `<home>/auth.lock`. Writers MUST reload `secrets.json` under that lock. Writers MUST merge mappings by secret value against the reloaded set. A colliding placeholder MUST be reassigned during the merge. Writers MUST allocate new placeholders against the merged set. Writers MUST refresh the in-memory map after the write. |
| 15 | The exec and patch tools MUST unmask with `UnmaskStrict` immediately before execution or resolution. The exec and patch tools MUST fail the call without writing or running when a placeholder with masker grammar has no known value. |
| 17 | A restore that encounters a secret placeholder with masker grammar and no known value MUST fail closed. Recovery MUST abort with an error without writing placeholder bytes. Snapshot opens MUST abort with an error without populating the store. |
| 17 | The masker grammar includes `{{secret:<ENV_NAME>}}`, where the name ends in `_API_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD`. The masker grammar includes `{{secret:auth:<provider>}}`, with an optional `:access` or `:refresh` suffix. The masker grammar includes `{{secret:pattern<i>:<n>}}`, where both indexes are decimal. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 4 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/secrets.md` prints `style findings: 0`, and `just verify` passes.

