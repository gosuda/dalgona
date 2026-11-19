#### Spec-fix step 1. GrantPrefix rejects an unterminated quote or a trailing backslash
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/approval-rules.md` line 9 says: "Malformed or empty input MUST produce no grant." Keep that sentence.

- Where: `internal/agent/rules/rules.go:tokenize`, `internal/agent/rules/rules.go:GrantPrefix`.
- Change: Change `tokenize` to return `([]string, bool)`. It returns `false` when `scanner.quote` is not zero or `scanner.escaped` is true after the last rune. `GrantPrefix` returns ("", false) when `tokenize` returns `false`. Keep the existing empty-token and compound checks. No error text changes.
- Failing-first test: `TestGrantPrefixRejectsUnterminatedQuote` in `internal/agent/rules/grant_prefix_malformed_test.go (new)`. Input: GrantPrefix(`echo "abc`) Expected: prefix is "" and ok is false The expected value comes from: docs/spec/approval-rules.md:9 `Malformed or empty input MUST produce no grant.`; the expected `false` is the spec's no-grant result, not the current output
- Evidence at commit `3c4b6f94`: internal/agent/rules/rules.go:61-64 `type shellScanner struct { quote rune; escaped bool }`; rules.go:66-96 `advance` and `advanceQuoted` leave `quote` set on an open quote; rules.go:218-230 `tokenize` ignores the scanner state at the end; rules.go:190-203 `GrantPrefix` accepts the result. Current result for `echo "abc`: prefix `echo abc`, ok true.
- Proof: the test fails before the change and passes after it, and `just verify` passes.

