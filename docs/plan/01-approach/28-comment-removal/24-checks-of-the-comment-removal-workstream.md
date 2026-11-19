#### Checks of the comment-removal workstream

- After each step: `just verify` (`justfile:121`) passes at that commit. Step 1 changes documentation only.
- After each `document` row: the sentence appears once in the named file, has at most 25 words and has no semicolon.
- After each `delete` row: `grep -rn -F` for the first words in the named file or package returns no line.
- After Step 3: `grep -rn "CheckValue" internal cmd docs` returns no line.
- After Step 19: `grep -n '#' docs/spec/hashline/verify_model.py` and `grep -n '"""' docs/spec/hashline/verify_model.py` each return no line.
- After Step 20: `grep -n '^[[:space:]]*//' web/src/routes/+page.svelte` returns no line.
- After Step 21: `go test -race -count=1 ./internal/commentgate -v` passes, and `TestRepositoryHasNoProseComments` reports zero findings.
- The row counts add up: 72 Go rows, 8 Python rows and 1 Svelte row, 81 in total.
- When `just spec-style` exists (the Specifications workstream, step 40), it runs with each step that adds `docs/spec` sentences and reports zero findings.

