#### Edit-review step 3. Config table `[edit_review]`
Anchor: Dieter Rams

- Files and symbols:
  - `internal/config/config.go`: `EditReviewConfig struct{ Enabled bool; Feedback []string }` after `ApprovalDeciderConfig` (lines 200-203), and the `Config` field `EditReview EditReviewConfig` after `ApprovalDecider` (line 239). A nil `Feedback` means the key is absent. This step is the only declaration of `Config.EditReview`. The protocol workstream, Step 8 reads `Config.EditReview.Enabled` and declares no field.
  - `internal/config/merge.go`:
    - `fileDoc` gets `EditReview editReviewDoc` with tag `toml:"edit_review"` after `ApprovalDecider` (line 44).
    - `editReviewDoc struct { Enabled *bool `toml:"enabled"`; Feedback []string `toml:"feedback"` }`, next to `herdrDoc` (line 200).
    - In `mergeTables`, after the `ApprovalDecider` block (lines 334-337): `if doc.EditReview.Enabled != nil { c.EditReview.Enabled = *doc.EditReview.Enabled }`, and `if doc.EditReview.Feedback != nil { c.EditReview.Feedback = doc.EditReview.Feedback }`. The nil test matches `SandboxWritableRoots` (lines 238-240).
    - In `dalgonaOnlyKeys` (line 530), add `"edit_review": true`.
  - `internal/app/app.go` line 91: add `EditFeedback: cfg.EditReview.Feedback,` to the `agent.HostConfig` literal. `internal/agent/types.go` `HostConfig` (line 62): add `EditFeedback []string` after `Config` (line 64).
  - The `feedback` id check is not in this step. Step 4 runs it in `NewHost`, with the question table that Step 4 creates.
- Behaviour:
  - `enabled` defaults to `false`. The table is dalgona only. dal rejects it.
  - `feedback` absent: no question speaks until Step 9 writes the measured speakers into `reviewBuiltInSpeakers`. `feedback = []` present and empty: no question speaks. No feedback block reaches the model, but verdicts still journal and show.
  - `settings.toml` overrides `config.toml` per key, as for the other keys.
- Failing-first tests:
  - `TestLoad_editReview` (`internal/config/config_test.go`, new, after `TestLoad_approvalDecider` at lines 202-216): the default is `Enabled` false and `Feedback` nil. `[edit_review]\nenabled = true\n` gives `true`. `enabled = "yes"` fails to load. A `settings.toml` `enabled = false` overrides a `config.toml` `true`. `feedback = ["lost_code", "wrong_logic"]` gives those two ids in that order. `feedback = []` gives a non-nil empty list. `feedback = "lost_code"` fails to load. Before this step, the test fails with an unknown-key error.
  - `TestLoad_dalRejectsDalgonaSettings` (`config_test.go:218-239`): add `{key: "edit_review", body: "[edit_review]\nenabled = false\n"}` after the herdr entry (line 231). It expects `config.toml:1: edit_review is a dalgona setting`, which `merge.go:548` produces.
- Spec: `docs/spec/config.md`. Add `edit_review` to the list of dalgona-only settings in the paragraph that begins `The loader MUST reject unknown keys.`. Add one paragraph after the paragraph that begins `The decider MUST start with`, with the two keys, the TOML example from the section "Schemas of the edit-review workstream", and the rule that `sound` is not a speaker. This step owns the `[edit_review]` text of that file. It covers the file key only. The wire key `editReview` belongs to the protocol workstream.
- Lands after: none. This step lands in phase 7, before the protocol workstream, Step 8.

