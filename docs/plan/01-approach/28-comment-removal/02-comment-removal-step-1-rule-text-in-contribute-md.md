#### Comment removal step 1. Rule text in CONTRIBUTE.md and REVIEW.md

Anchor: Dieter Rams

- Files: `CONTRIBUTE.md` and `REVIEW.md`.
- Change: In `CONTRIBUTE.md`, in the paragraph that starts `Use names, types, and control flow`, replace the two sentences `Keep required build directives.` and `Use a source citation when it explains a non-obvious protocol rule.` with the three rule sentences below. Under `## Prove the behavior`, add the exception paragraph below as a new paragraph after the paragraph that starts `Make each test detect`. In `REVIEW.md`, add the one sentence below at the end of the paragraph that starts `A finding SHOULD include`.
- Rule sentences for `CONTRIBUTE.md`: `Write no prose comment in Go, hand-written TypeScript, Svelte, or Python source.` `Keep only these Go directives: `//go:build`, `//go:embed`, `//go:generate`, and `//nolint:<linters> // <reason>`.` `Put the source of a non-obvious protocol rule in `docs/spec`, with the upstream project and file named.`
- Exception paragraph: `The comment gate, `TestRepositoryHasNoProseComments` in `internal/commentgate`, is the one named exception to the rule above on source text. It MAY read source text to find prose comments, and its findings are not proof of behavior.`
- Sentence for `REVIEW.md`: `The one exception is the comment gate that `CONTRIBUTE.md` names.`
- Tests that fail before the change: none. The step changes documentation only. Until Step 21 the tree still holds comments, so the rule describes the target that the gate enforces later.
- Check: `just verify` passes. `grep -n "source citation" CONTRIBUTE.md` returns no line. `grep -n "one named exception" CONTRIBUTE.md` returns one line.
- Lands after: nothing

