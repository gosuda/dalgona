# Secret masking

## Invariant

Raw secret values covered by the masker MUST NOT enter the journal or a model request. Reversible placeholders MUST enter those surfaces instead. Values shorter than eight bytes are outside the masking contract.

Exec, patch, and rewrite MUST restore known placeholders at the execution or edit-resolution boundary. Rewrite MUST unmask only `pattern` and `replace`, never `path` or `glob`. Other tools MUST NOT gain a general unmask step. Text that does not match a secret placeholder format MUST remain unchanged, and a well-formed placeholder that cannot be restored MUST return an error. Pattern mappings MUST persist before masked text leaves the masker. A persistence failure MUST return an error, not a raw secret or an unrecoverable placeholder.

Recovery restores pre-edit file images and snapshot blobs through the strict unmask step.

## Scope

This decision applies to credentials, configured secret patterns, prompts, tool output, and the exec, patch, and rewrite tools. [secrets.md](../spec/secrets.md) defines placeholder formats and file permissions. Masking is not a claim that every possible secret string can be detected.

## Enforcement

`internal/dal/auth/auth_test.go` checks longest matches, short values, placeholder stability, OAuth field separation, persistence, and failure retry. `TestSecretsAreMaskedInJournalAndUnmaskedInExec` in `internal/dal/agent/misc_test.go` checks the journal and model boundary. `internal/dal/tool/patch/property_test.go` checks locator and body restoration without path substitution. `just test` runs these checks.
