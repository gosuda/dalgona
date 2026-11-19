#### Spec-fix step 7. Reject NUL bytes anywhere in the file
Anchor: Tony Hoare, Richard Feynman

The text is right and the code breaks it. `docs/spec/edit-styles.md` line 17 says: "The resolver MUST reject NUL bytes." Keep that sentence.

- Where: `internal/tool/patch/text.go:decodeDocument`.
- Change: Replace raw[:min(len(raw), 8192)] with raw in the bytes.IndexByte test at text.go:25. Keep the utf8.Valid test and the error text at text.go:26.
- Failing-first test: `TestDecodeDocumentRejectsNULPastFirstKiB` in `internal/tool/patch/text_test.go (new file)`. Input: path f.txt; content is 9000 bytes of the letter a, then one NUL byte (0x00), then a newline Expected: decodeDocument returns an error whose text is `patch: f.txt is not a text file (binary or invalid UTF-8)` The expected value comes from: The text is the existing message at text.go:26, which the NUL-in-first-8192-bytes case already returns. The rule is spec line 17.
- Evidence at commit `3c4b6f94`: internal/tool/patch/text.go:25 `if bytes.IndexByte(raw[:min(len(raw), 8192)], 0) >= 0 || !utf8.Valid(raw) {`. NUL is valid UTF-8, so utf8.Valid does not catch it.
- Proof: the test fails before the change and passes after it, and `just verify` passes.

