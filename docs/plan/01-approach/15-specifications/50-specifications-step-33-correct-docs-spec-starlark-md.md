#### Specifications step 33. Correct `docs/spec/starlark.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/starlark.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 53 | `Judge` MUST ask the decider for `req.Model`, or for the resolved `[approval] decider` when the model is empty. | `Judge` MUST ask the decider for `req.Model`, or for the resolved `[approval_decider] decider` when the model is empty. | internal/app/decider.go:90-92 `func (j *judges) judge(ctx context.Context, modelSpec, threadModel string)` with `spec := j.spec` and `if modelSpec != "" {`; internal/config/merge.go:44 `ApprovalDecider approvalDeciderDoc` with tag `toml:"approval_decider"`; in |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 97 | A `data` value that cannot decode MUST fail with status `failed`, code `invalid_args`, and stage `args`. | internal/script/eval.go:126 `return stopped(StatusFailed, codeInvalid, "eval: data: "+err.Error(), stageArgs)` after `data, err := fromJSON(req.data)` in `execute` |
| 137 | A workflow whose `args` cannot decode MUST fail with status `failed`, code `invalid_args`, and stage `args`. | internal/script/workflow.go:29-30 `args, err := fromJSON(start.Args)` then `return stopped(StatusFailed, codeInvalid, "workflow args: "+err.Error(), stageArgs)` |

Structure:

- Confirmed and unchanged: step limit 1,000,000,000 (run.go:22), print caps (run.go:23-24), scope limit 1 to 64 (ctx.go:222-224), `ctx.spawn` 8 children (internal/agent/spawn.go:17 `maxChildren = 8`, extension.go:120-124), `ctx.judge_batch` 4096 states (judge.go:91-92 with ctx.go:20 `maxPending = 4096`), judge default concurrency 8 (judge.go:14), eval reply keys (eval.go:213-221 and eval.go:255-257 `truncated`).
- Journal bullets confirmed in internal/scripthost/effects.go: Started (effects.go:121 duplicate-run error and pinned start), Record (effects.go:299-310 journals script.effect), Completed (effects.go:321-339), CallTool (effects.go:341-373 with script.ErrDenied wrap), Spawn (effects.go:375-381 records the quoted child id), Wait (effects.go:383-393 input order), child.spawned asserted in internal/agent/childcap_test.go:83-84.
- Verified in this run. `Completed` runs once per run end. `complete` (effects.go:127-131) calls `fx.Completed`, and the three run-end call sites return through it (eval.go:89, plugin.go:190, workflow.go:21). The judge reply is an object keyed by question name (judge.go:74-87). Each answer holds `choice` when set (judge.go:91-92), `noul` and `score` when non-nil (judge.go:94-95), and `probabilities` with `confidence` only when `probabilities` is non-empty (judge.go:96-106). The host journals `approval.resolved` for a hook decision. `decide` returns the hook verdict first (gate.go:89-90), and `gate` appends the resolved event for every verdict (gate.go:64-66). A prior-turn run id starts fresh. `ensureTurn` resets the cache when the turn changes (effects.go:79-87), `scopedRecords` starts at the latest turn start (effects.go:71-77), and the lookup in `effectCache.start` finds no start for that run (effects.go:224-243). `LoadPlugins` lists `plugins/*.star` with `filepath.Glob` (plugin.go:66-67). `filepath.Glob` sorts each directory listing (/usr/lib/go/src/path/filepath/match.go:319-336, the installed Go library, outside the repository).
- Verified: when Ensure cannot reach the daemon before its timeout, `dalgona run` prints `dalgona: daemon did not start: see <home>/daemon.log` to stderr and exits 1 (run.go:49, remote.go:144-150, cmd/dalgona/daemon.go:36-41, internal/daemon/daemon.go:59 and 102-109, main.go:19 and 108-110).
- The LoadPlugins keep-valid-file rule is confirmed: plugin.go:74-75 appends one wrapped error per failing file and continues, and loadFile keeps only registrations that pass the uniqueness check.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 1 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/starlark.md` prints `style findings: 0`, and `just verify` passes.

