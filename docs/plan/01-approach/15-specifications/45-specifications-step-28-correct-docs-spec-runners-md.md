#### Specifications step 28. Correct `docs/spec/runners.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/runners.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 17 | Every pipe service MUST receive `runner.Bootstrap`. | Every service except `cloudflare` MUST receive `runner.Bootstrap`. | internal/remote/remote.go:153-173 `defaultService` passes runner.Bootstrap to docker, sprites, modal and sandbox0; remote.go:193-202 `cloudflareService` calls `cloudflare.New(spec.URL, token)` without it |
| 80 | Pull MUST NOT move the baseline. A second pull diffs against the same baseline. The plan does not specify this behavior. | Pull MUST NOT move the baseline. A second pull MUST diff against the same baseline. | internal/remote/pull.go Pull and pullInto (read in this run) write no baseline; boxDiff passes the baseline argument to diffScript (pull.go:13) |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 71 | The box writes the diff to `$HOME/.dalgona/pull.diff` and prints that path. | internal/remote/pull.go:13 diffScript writes the diff to $HOME/.dalgona/pull.diff and prints $out; pull.go boxDiff reads the printed path with r.ReadFile |

Use one word for one thing.

- Keep `box`. Replace sandbox boxes at lines 3.
- Keep `Options`. Replace remote.Options at lines 103, 116, 117, 118.
- Keep `*Binder`. Replace *remote.Binder at lines 103.
- Keep `Recorder`. Replace remote.Recorder at lines 104.
- Keep `Box`. Replace remote.Box at lines 108.
- Keep `IO`. Replace remote.IO at lines 116, 117, 118.

Structure:

- Entry-point names are package-local: remote.go:73 `func NewBinder(opts Options) *Binder`; remote.go:40 `type Recorder interface`; lifecycle.go:87 `GC` returns `[]Box`; the doc's remote. prefixes are the wrong qualifier inside package remote.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 27 | When Create fails, the binder MUST attach a box by that name. That attach recovers a box left by a crash before `runner.bound` was recorded. |
| 31 | When the file is missing, the binder MUST upload the workspace again. The binder MUST record `runner.bound` with the new baseline. The binder MUST write this line to `Options.Out` and log it at warn level: |
| 92 | The new `box_id` MUST be recorded right after Create, before attach or bootstrap. A failed attach then leaves a box that `cloud up` and `cloud down` can reach. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 4 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/runners.md` prints `style findings: 0`, and `just verify` passes.

