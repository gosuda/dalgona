### Workstream: Ratchet tool

#### Outcome of the ratchet workstream

In dalgona, a user can type `ratchet` and the model builds a trusted eval for one LLM flow. The host keeps the flow's state under `<cwd>/.dalgona/ratchet/<flow>/`. That state holds a seeded, stratified train/test split that is frozen at the baseline. It also holds approvals that each bind a content hash, and a gate that returns `keep`, `revert` or `rerun` from a paired case-level 95% interval on both splits. The model talks to all of this through one `ratchet` tool. Approvals reach the human through the ask wire `item/tool/requestUserInput` and are refused when no client is attached. Each round also records a digest of the files it changed, a plateau counter and a round ceiling. Separately, both binaries gain two taste notices, `decide` and `simplify`. A guard test now stops any built-in keyword from shipping with an empty notice.

#### Decisions of the ratchet workstream

| # | Decision | Rival rejected, and why |
|---|---|---|
| D1 | One battery package `internal/ratchet` holds the gate logic (unexported), the `ratchet` tool, and the `Asker` seam. It imports only the standard library, `internal/tool` and `internal/runner`. | `internal/tool/ratchet` under the core tree: the core depguard rule (`.golangci.yml:99-132`) treats `internal/tool/**` as core, and `internal/agent` would link it into `dal`. |
| D2 | The tool is registered from the battery's `Tools` function (`internal/app/battery/battery.go:73-77`, `:134-136`). The battery reads the thread with `agent.ThreadFromContext` (`internal/agent/workspace_trust.go:150-153`). That works because `buildTools` passes `ContextWithThread(ctx, t)` (`internal/agent/thread.go:263`). Only top-level, non-advisor threads get the tool. | `withSubagentTools` (renamed `withThreadTools` by the ask workstream) in `internal/agent/spawn.go:39-50`: it is core, so `dal` would carry a battery. |
| D3 | One tool, `ratchet`, with an `action` field taking `init`, `plan`, `split`, `approve`, `check`, `gate`, `train` or `status`. | Eight separate tools: they bloat every request's tool list for a feature most turns never use. |
| D4 | Tool class `tool.ClassRead`. The gate auto-accepts it (`internal/agent/gate.go:128`) and recovery re-runs it (`internal/agent/recover.go:151`, `docs/spec/recovery.md:30`). Every action is made safe to re-run: `gate` replays a recorded round when the results digest has not changed, `split` without `seed` takes its seed from the call id, and `approve` returns without asking when the approval is already current. A recovered `approve` whose question was open ends with `ok=false` and `recovered=interrupted`, as `ask` does (ask D3), and it does not ask again on restart. | `ClassScript` or `ClassEdit`: their recovery paths assume an engine run or a planned file edit, and the ratchet has neither. |
| D5 | The statistic is the paired case-level mean difference with a two-sided 95% Student t half-width. Critical values cover df 1 to 30, and above that the value is 1.96. This is the rule in the original at `.references/oh-my-pi/packages/coding-agent/src/ratchet/ratchet.ts:111-133` (meanAndHalf and the T table) and `:632` (paired). | Bootstrap: it is random and needs a second seed. A sign test ignores effect size. |
| D6 | The keep, revert or rerun rule matches the original's decision table (`ratchet.ts:884-903`). Two additions: a variant may be gated `rerun` at most 2 times, and after that a rerun becomes `revert`. A flow with a pending rerun accepts no other variant. | No rerun limit: the original leaves rerun loops unbounded. |
| D7 | The round ceiling is always set: `stop.rounds` defaults to 10 (range 1 to 50) and `stop.plateau` defaults to 3 (range 2 to 10). Once `done` is true, `check` refuses new variants until a new plan is approved. | An optional ceiling (the original's `stop.rounds?`): the loop could then run without limit. |
| D8 | The change digest has three parts. `check(vN)` records `pending = {variant, change_sha}`. `gate(vN)` refuses when the change paths differ from the pending digest. Each round stores `change_sha`, the full per-file list `change_files`, and `changed_files` (the paths that differ from the best round). `check` refuses a non-baseline variant whose change digest equals the best round's. | Recording only the path names, as the original's plan digest does (`ratchet.ts:402`): edits made in the middle of a run and empty rounds would go unseen. |
| D9 | Held-out leak checks: (a) no trace file for a test case; (b) every file in `traces/` must match `<case>_rep<k>.json`; (c) a test row in `results.jsonl` may carry no string member except `case_id`, `status` and `model`, plus the `tags` array. | Trace checks alone, as in the original: test outputs could still reach the analyzer through `explanation` or `output` fields in `results.jsonl`. |
| D10 | Result rows use `case_id`, not `prompt_id`. The state file is `state.json`. No `status.md` is written; `status` returns the table. | Keeping the claude-api layout (`ratchet.ts:8-11`): dalgona has no report builder that reads it. |
| D11 | `approve` asks one question with options `Approve` (recommended, first, and carrying the preview), `Revise` and `Abort`, plus the automatic "Other" row for free-text feedback. The preview opens with a block the host builds itself (stage, digest, and the paths or plan facts), followed by the model's preview. | A model-only preview: the human would not see the exact facts the hash binds. |
| D12 | `decide` and `simplify` are enabled in both products and require no tools. `ratchet` is dalgona only and requires `ratchet`, `exec` and `task`. | `decide` and `simplify` in dalgona only: the notices are plain text and need no battery. |
| D13 | `notice(word)` reads `notices/<word>.md` and panics when the file is missing or empty. A guard test checks that the built-in table matches `notices/*.md` one to one. | Returning `""` (`internal/keywords/keywords.go:110-116`): a missing file silently ships a keyword with no notice. Returning an error from `Builtins`: every caller's signature changes for what is a build-time defect. |
| D14 | `state.json`, the result files and the tool arguments are decoded with `encoding/json/v2`. This is cold data read once per call, which `docs/decisions/dependencies.md` line 6 permits ("`encoding/json/v2` reflection MAY decode or encode cold data that the program reads once"), and `internal/agent/gate.go:7` already does the same. Files are read and written through `os.Root` opened at the thread cwd. The state lives in the workspace, so the model's own edit tools can change it. The gate detects a changed plan or approval but cannot stop a change to `state.json` itself, and the spec must state this limit. | jsontext streaming for about 20 argument fields: much more code on a cold path. |

##### Left out, and why

- Pricing catalogues (`priceFor` at `ratchet.ts:541`, `rowCost` at `:555`, the `PriceLookup` type at `:95`): they tie the gate to a model registry. A runner that needs a cost guardrail writes `cost_usd` on each row, and the gate treats it as a numeric field whose better direction is lower.
- A second language facade (the original ships matching Python and JS preludes, `.references/oh-my-pi/packages/coding-agent/src/ratchet/prelude.py:1-81` and `.references/oh-my-pi/packages/coding-agent/src/ratchet/prelude.js:1-30`): dalgona has one tool schema. `eval` cells reach it as `ctx.tools.ratchet(...)` (`docs/spec/starlark.md` "The ctx object").
- Narrative reports (`narrative.md`, the `[REQUIRED]`/`[TUNE]` tags, the stall taxonomy, `status.md`): they are prompt-only prose that the host cannot check. The notice asks for one status line per round and a final test change with its interval. `status` returns the table.
- A judge inside the gate: grading stays in the user's harness. A harness may use `ctx.judge_batch` (`internal/script/judge.go:58`, `internal/scripthost/judge.go:14-35`), but the gate only reads `grade`.

##### Licence

`.references/oh-my-pi/LICENSE:1-5` is MIT, copyright "2025 Mario Zechner", "2025-2026 Can Bölük" and "2026 Stencil Labs, Inc.". This port re-derives in Go the original's decision table, statistic, split algorithm, digest scheme and refusal set. Treat it as a substantial portion and carry the notice:

1. Ratchet step 3 (order row 139), the first commit that ports code, adds a new root file `THIRD_PARTY_NOTICES.md`. Its heading is `# Third-party notices`, followed by the section `## oh-my-pi (MIT)`. The section names the ported file `packages/coding-agent/src/ratchet/ratchet.ts` from oh-my-pi (https://omp.sh), states that `internal/ratchet` adapts it with changes, then reproduces the three copyright lines and the full MIT permission and warranty text verbatim from `.references/oh-my-pi/LICENSE:1-23`.
2. In the `dist` recipe (`justfile:102-114`), add `cp THIRD_PARTY_NOTICES.md dist/` after `mkdir -p dist`, so the notice ships with the binaries.
3. `docs/decisions/ratchet.md` links to the file.
4. No licence comment goes in the Go source. The notice stays in `THIRD_PARTY_NOTICES.md`.
5. `prompts/tools/ratchet.md` does not exist at `.references/oh-my-pi/packages/coding-agent/prompts/tools/ratchet.md`, so this item cites no text from it. The worker MUST NOT copy any sentence from any prompt or skill file, public or private. The notices in step 8 and step 2 are new text, which is why no Apache NOTICE obligation arises.

Each step is one commit. Run `just verify` before each commit.

#### Ratchet step 1. Notice loading cannot be empty

- `internal/keywords/keywords.go:110-116`: replace `notice(name string) string` with `notice(word string) string`. It reads `"notices/" + word + ".md"`. On a read error it panics with `"keywords: missing notice notices/" + word + ".md: " + err.Error()`. When the trimmed text is empty it panics with `"keywords: empty notice notices/" + word + ".md"`.
- In `builtinTable` (`:22-29`; the rows at `:24-27` pass the names with `.md` today), change the four calls to `notice("ultrathink")`, `notice("jevify")`, `notice("workflowz")` and `notice("orchestrate")`.
- New test `TestEveryBuiltinHasItsNotice` in `internal/keywords/builtins_test.go`:
  - `os.ReadDir("notices")` gives the stems of the `*.md` files.
  - `keywords.Builtins("dalgona")` gives the words.
  - Assert the two sets are equal, sorted.
  - For each keyword, read `notices/<word>.md` with `os.ReadFile`, require a nil error, and assert `Notice` is not empty and equals `strings.TrimSpace(string(data))`.
  - It fails before the change when a table row has no file. Run it red first: add a table row for the word `scratch` with no notice file, run the test, see it fail, then remove that row; the existing `checkBuiltin` (`keywords_test.go:43-57`) cannot see orphan files.
- `docs/spec/keywords.md`, under `## Keyword table`, after the sentence that begins `The package MUST embed every notice`: append "Each built-in MUST read `notices/<word>.md`. A missing or empty notice MUST stop the program when the table is built. Every file in `notices/` MUST belong to a built-in." The protocol workstream, Step 7 owns the Spans and Usable text in this file, so this step adds only this sentence.
- Anchor: Tony Hoare.

#### Ratchet step 2. Taste notices `decide` and `simplify`

- In `builtinTable`, append these rows after `orchestrate`:
  - `{Word: "decide", Hue: [2]int{200, 250}, Effort: "default", Notice: notice("decide"), Enabled: true}`
  - `{Word: "simplify", Hue: [2]int{90, 140}, Effort: "default", Notice: notice("simplify"), Enabled: true}`
- New file `internal/keywords/notices/decide.md`, exactly:

  ```
  Make the call instead of surveying it. Name each real option in one line. Recommend one option and give the reason that decides it. State the fact that would change your recommendation. Do not add an option only to avoid choosing. Then act on the recommendation, unless the user has to approve it first.
  ```

- New file `internal/keywords/notices/simplify.md`, exactly:

  ```
  Delete before you add. Remove dead code, unused settings and wrappers that only pass calls through. Keep every validation, error path and piece of evidence that someone relies on. Prefer one direct function over a new layer. Add no helper, flag or dependency without a current need. List what you removed and why removing it was safe.
  ```

- Tests:
  - `TestBuiltinsProductMetadata` (`keywords_test.go:13-34`): `dal` wants `ultrathink`, `decide`, `simplify`; `dalgona` wants all six, in table order, with the hues above.
  - `TestBuiltinsNotices` (`:59-81`): add `"decide": {"option", "Recommend", "change your recommendation"}` and `"simplify": {"Delete", "validation", "error path", "evidence"}`.
  - `TestDefaultEnabled` (`builtins_test.go:9-29`): add `{"dal","decide",true}` and `{"dal","simplify",true}`.
  - These fail before the change.
- `docs/spec/keywords.md`:
  - Under `## Keyword table`, replace the sentence that begins `Builtins("dal")` MUST return only with "`Builtins("dal")` MUST return `ultrathink`, `decide` and `simplify`. `Builtins("dalgona")` MUST return every built-in in table order."
  - Add the two table rows after the `orchestrate` row.
  - Add these contracts after the `orchestrate` bullet under `## Notices`: "`decide`: name the options, recommend one with the deciding reason, and state what would change the recommendation." and "`simplify`: delete first, and keep validation, error paths and evidence."
- Lands after: Step 1 (`notice(word)` and the guard test).
- Anchor: Dieter Rams, Kenya Hara.

#### Ratchet step 3. Statistic, decision rule, plateau and ceiling (`internal/ratchet`, pure)

Ports from `ratchet.ts`. This step adds the `## oh-my-pi (MIT)` section to `THIRD_PARTY_NOTICES.md` (Licence, item 1). It is the first step that ports code (cross-phase decision 8), so the section is added here once; steps 4 to 6 change no notice text.

New files in package `ratchet`:

- `stats.go`:
  - `type summary struct { N int; Mean, Half *float64 }`
  - `type delta struct { summary; Verdict string }`
  - `var tCritical95 = [30]float64{12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160, 2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042}`
  - `func tCritical(df int) float64`: returns 1.96 above 30.
  - `func meanAndHalf(values []float64) summary`: n=0 gives nil mean; n=1 gives nil half; otherwise the sample variance uses `n-1` and `half = t(n-1)*sqrt(var/n)`.
  - `func paired(cand, ref caseMeans, ids []string, direction string) delta`: diffs over ids present in both. `n` counts those ids. `caseMeans` is `type caseMeans map[string]float64`, the mean over reps of one metric for each case id, built in step 5 by `meansByCase`.
  - `func verdict(s summary, direction string) string`: let `gain` be `mean` when higher and `-mean` when lower. The result is `up` when `gain-half > 0`, `down` when `gain+half < 0`, and `flat` otherwise, including when N < 2.
- `rule.go`:
  - `type pair struct{ Train, Test delta }`
  - `type guard struct{ Metric string; pair }`
  - `func decideRound(target string, t pair, guards []guard, ref, variant string, reruns int) (string, string)`: returns the decision and the reason. `ref` is the best variant, `baseline` until a `keep`. The checks run in this order: (1) a guard whose train or test verdict is `down` gives `revert` with `guardrail regressed vs <ref>: <metrics>`, where `<metrics>` lists each such guard in hold order, joined by `, ` (as `ratchet.ts:884-886` does); (2) a target whose train or test verdict is `down` gives `revert` with `<target> regressed vs <ref>`; (3) a target `up` on both splits gives `keep` with `<target> improved on train and test vs <ref>`; (4) a target train verdict `up` gives `revert` with `overfit suspect: train improved but test is flat vs <ref>`; (5) a target test verdict `up` gives `rerun` with `test improved but train did not; add reps to <variant> and <ref>, then gate <variant> again`; (6) anything else gives `revert` with `no change outside noise vs <ref>`. The table below uses `score` as the target. When the decision is `rerun` and `reruns` is 2 or more, it returns `revert` with `rerun limit reached: <variant> was rerun 2 times` (`maxReruns = 2`).
  - `func (s *state) advance(decision string)`: `keep` and `baseline` reset `FlatRounds` to 0; `revert` adds 1; `rerun` leaves it.
  - `func (s *state) done() (bool, string)`: false when the last round is `rerun`. Otherwise `"plateau after N reverts"` when `FlatRounds >= Stop.Plateau`, or `"round ceiling N reached"` when the count of non-baseline rounds is at least `Stop.Rounds`.
- `state.go`: `type stopRule struct{ Plateau, Rounds int }`, `type round struct{ Round int; Variant, Decision string; Reruns int }` and `type state struct{ Stop stopRule; Rounds []round; FlatRounds int }`. Step 4 adds the other `state.json` fields with snake_case JSON tags.
- The tests in this step are written first. They fail to compile until the functions exist.
- `stats_test.go`, `TestMeanAndHalfAndVerdict`, tolerance 1e-4, values computed by hand:

| Values (diffs) | Direction | n | Mean | Half | Verdict |
|---|---|---|---|---|---|
| [1,1,1,0] | higher | 4 | 0.75 | 0.7955 (s²=0.25, se=0.25, t3=3.182) | flat |
| [1,1,1,1,0,1] | higher | 6 | 0.83333 | 0.42850 (s²=0.16667, se=0.16667, t5=2.571) | up |
| [0.5,0.5,0.5] | higher | 3 | 0.5 | 0 | up |
| [0.2,0.2,0.3,0.3] | lower | 4 | 0.25 | 0.09186 (s²=0.003333, se=0.028868) | down |
| [-0.2,-0.2,-0.3,-0.3] | lower | 4 | -0.25 | 0.09186 | up |
| [1] | higher | 1 | 1 | nil | flat |
| [0,0,0] | higher | 3 | 0 | 0 | flat |
| [] | higher | 0 | nil | nil | flat |
| 16 × 1 then 16 × 0 | higher | 32 | 0.5 | 0.17601 (s²=8/31, se=0.089803, t=1.96) | up |

- Also `TestTCritical`: 1 gives 12.706, 30 gives 2.042, 31 gives 1.96.
- Also `TestPairedSkipsUnpaired`: cand `{a:1,b:1,c:1}`, ref `{a:0,b:0,d:0}`, ids `[a,b,c,d]` give n=2, mean 1, half 0, `up`.
- `rule_test.go`, `TestDecideRound` (ref `baseline`, variant `v1`, target `score`, guard `latency_s`):

| Guard train/test | Target train/test | Decision | Reason |
|---|---|---|---|
| flat/down | up/up | revert | `guardrail regressed vs baseline: latency_s` |
| down/flat | up/up | revert | same |
| flat/flat | down/up | revert | `score regressed vs baseline` |
| flat/flat | flat/down | revert | `score regressed vs baseline` |
| flat/flat | up/up | keep | `score improved on train and test vs baseline` |
| flat/flat | up/flat | revert | `overfit suspect: train improved but test is flat vs baseline` |
| flat/flat | flat/up | rerun | `test improved but train did not; add reps to v1 and baseline, then gate v1 again` |
| flat/flat | flat/flat | revert | `no change outside noise vs baseline` |

- `TestRerunLimit`, through `decideRound` with the variant's rerun count:
  - (`rerun`, reruns 1) stays `rerun`.
  - (`rerun`, reruns 2) becomes `revert` with `rerun limit reached: v1 was rerun 2 times`.
  - (`keep`, 2) stays `keep`.
- `TestPlateauAndCeiling`:
  - Plateau 3, rounds 10. Decisions baseline, revert, revert, keep, revert, rerun, revert, revert give `FlatRounds` 0,1,2,0,1,1,2,3. `done` is false through the 7th decision and true at the 8th with `plateau after 3 reverts`.
  - Plateau 3, rounds 2. Decisions baseline, revert (v1), rerun (v2), keep (v2, which replaces the rerun round) give done false, false, false, then true with `round ceiling 2 reached`.
- `docs/spec/ratchet.md`, new: sections "Statistic", "Decision" and "Stop" with the rules above in MUST form.
- `docs/decisions/ratchet.md`, new, with headings Invariant, Scope and Enforcement:
  - Invariant: the host owns the split, approvals, completeness, leak checks and the round decision; `dal` MUST NOT import `internal/ratchet`.
  - Scope: `internal/ratchet`, `internal/app/battery`, `.golangci.yml`.
  - Enforcement: the depguard `core` and `dal` rules, and the tests named in this workstream.
  - Licence: link to `THIRD_PARTY_NOTICES.md`.
- `.golangci.yml`: the edit-review workstream, Step 1 owns the `core` files list and lands first, so this step owns only the two ratchet deny entries. Add `- pkg: "github.com/gosuda/dalgona/internal/ratchet"` with `desc: core must not import interfaces or batteries` to the `core` deny list, and with `desc: dal must stay free of dalgona batteries` to the `dal` deny list.
- Add `THIRD_PARTY_NOTICES.md` (Licence, item 1). In the `dist` recipe, add `cp THIRD_PARTY_NOTICES.md dist/` on the line after `mkdir -p dist`. This step owns the `dist` recipe. The live-debug workstream, Step 3 adds its own recipes to the same file, and this step does not edit them.
- `docs/README.md`: add an index entry under "State and execution": "[Ratchet](spec/ratchet.md): held-out eval flows, approvals, and the keep, revert or rerun gate." Under "Decisions": "[Ratchet](decisions/ratchet.md): host-owned eval gate in the dalgona battery."
- Lands after: the edit-review workstream, Step 1 (`.golangci.yml`).
- Anchor: Claude Shannon, Edsger W. Dijkstra.

#### Ratchet step 4. Flow state, setup, split, digests and approval status

Ports from `ratchet.ts`. The `## oh-my-pi (MIT)` section is already added in step 3.

- `flow.go`:
  - `const flowRoot = ".dalgona/ratchet"`.
  - `var flowName = regexp.MustCompile("^[a-z0-9][a-z0-9_-]{0,63}$")`, `var caseID = regexp.MustCompile("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")`, `var variantName = regexp.MustCompile("^(baseline|v[1-9][0-9]{0,3})$")`.
  - `type flow struct { root *os.Root; name, dir string }`. `func openFlow(cwd, name string) (*flow, error)` uses `os.OpenRoot(cwd)`.
  - `func (f *flow) load() (*state, error)` returns `errNoFlow` (`flow "<name>" does not exist; call init first`) when the file is absent.
  - `func (f *flow) save(s *state) error` writes `state.json.tmp` with mode 0o600 through `root.WriteFile`, then `root.Rename` to `state.json`, after `root.MkdirAll(dir, 0o750)`.
  - `func repoPath(p string) (string, error)` cleans the path to slash form and refuses `""`, absolute paths, `..` prefixes, and anything under `.dalgona/ratchet`.
- `setup.go`:
  - `func (f *flow) init(s *state, in initArgs) error`. It applies the refusals in the table below. Each change path is checked for overlap with every cases, harness and off-limits path. Overlap means equal, or one is a `/`-prefix of the other.
  - `func (s *state) applyPlan(p planArgs) error`.
  - `func (s *state) split(strata map[string]string, fraction float64, seed uint64) error`:
    - The keys of `strata` are the case ids. Each must match `caseID`, or the refusal in `TestSplitRefusals` applies.
    - Sort the tags. In each tag, sort the ids.
    - `r := rand.New(rand.NewPCG(seed, seed^0x9E3779B97F4A7C15))`, then `r.Shuffle` each bucket.
    - `k := int(math.Round(float64(len(bucket))*fraction))`; test gets `bucket[:k]`.
    - Move the last train id to test when test is empty. Move the last test id to train when train is empty.
    - Sort both lists and record `SplitSeed` and `TestFraction`.
- `digest.go`:
  - `func (f *flow) hashPaths(paths []string) (string, []fileSHA, error)`: returns the digest and the per-file list `{path, sha}`. The digest is SHA-256 over the sorted unique paths. A missing path writes `"missing:" + p + "\x00"`. Otherwise, for each regular file under the path in `fs.WalkDir(root.FS(), p)` order, it writes `rel + "\x00"`, then the file bytes streamed once with `io.Copy` into an `io.MultiWriter` that feeds both the digest and the file's own SHA-256, then `"\x00"`. More than 10000 files gives the error `<path> holds more than 10000 files`.
  - An absent path has per-file sha `""` (`docs/spec/recovery.md:44`).
  - `func (f *flow) digest(s *state, stage string) (string, error)`:
    - `inputs` hashes `CasesPaths` and `grader` hashes `HarnessPaths`.
    - `plan` refuses with `plan needs a target, a command and a split first` unless a target, a command and a non-empty split exist. `plan` defaults `reps` to 2, `plateau` to 3 and `rounds` to 10 when the call omits them. It then hashes `json.Marshal(planDigest{Goal, Reps, Stop, Command, ChangePaths, OffLimits, TrainIDs, TestIDs, SplitSeed}, json.Deterministic(true))`.
  - `func (f *flow) approvals(s *state) (map[string]string, error)` returns `missing`, `stale` or `current`.
- Tests:
  - `split_test.go`, `TestSplitCounts` (seed 7; written first, it fails to compile until `split` exists):

| Strata | Fraction | Train | Test |
|---|---|---|---|
| 5 `pos`, 3 `neg` | 0.4 | 5 | 3 (2 pos, 1 neg) |
| 4 `y`, 1 `x` | 0.4 | 3 (x stays train) | 2 |
| 4 `t` | 0.1 | 3 | 1 (moved) |
| 4 `t` | 0.9 | 1 (moved) | 3 |
| 5 `t` | 0.5 | 2 | 3 (`math.Round(2.5)=3`) |

  - `TestSplitRefusals`:
    - 3 cases give `split needs at least 4 cases`.
    - Fraction 0.05 gives `test_fraction must be from 0.1 to 0.9`.
    - Id `a/b` gives `case id "a/b" must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}`.
    - A state with a round gives `the split is frozen once the baseline is gated`.
    - Same seed twice gives identical id lists.
  - `digest_test.go`:
    - `TestHashPaths` builds file `cases.txt` = `"a\n"` and checks the digest equals `sha256("cases.txt\x00a\n\x00")`. A missing `gone.txt` gives `sha256("missing:gone.txt\x00")`. Directory `d/` with `d/x`=`"1"` and `d/y`=`"2"` gives `sha256("d/x\x001\x00d/y\x002\x00")`.
    - `TestPlanDigest`: the plan digest equals the SHA-256 of the literal `{"goal":{"target":"score","direction":"higher","hold":[],"directions":{}},"reps":2,"stop":{"plateau":3,"rounds":10},"command":"run {variant}","change_paths":["p.md"],"off_limits":[],"train_ids":["a","b","c"],"test_ids":["d"],"split_seed":7}`.
    - `TestApprovalGoesStale`: record `inputs` with the current digest, check `current`, edit the file, check `stale`.
  - `setup_test.go`, `TestInitRefusals`:

| Input | Error |
|---|---|
| cases `[]` | `init needs at least one cases path` |
| harness `[]` | `init needs at least one harness path` |
| change `[]` | `init needs at least one change path` |
| cases `../x` | `path must be inside the workspace: ../x` |
| cases `nope.txt` (absent) | `path does not exist: nope.txt` |
| change `eval` with cases `eval/cases.jsonl` | `change path eval overlaps eval/cases.jsonl; cases, harness and off-limits paths must stay out of the tuned surface` |
| off_limits `.dalgona/ratchet/x` | `path must stay outside .dalgona/ratchet: .dalgona/ratchet/x` |

  - `TestPlanRefusals`:

| Input | Error |
|---|---|
| hold includes target | `hold must not include the target metric` |
| reps 0 or 21 | `reps must be from 1 to 20` |
| reps 1 after baseline with reps 2 | `reps cannot shrink after the baseline` |
| plateau 1 | `plateau must be from 2 to 10` |
| rounds 51 | `rounds must be from 1 to 50` |
| command `run.sh` | `command must contain {variant}` |
| 9 hold metrics | `hold takes at most 8 metrics` |

  - Defaults: target `latency_s` with no direction gets `lower`; target `score` gets `higher`.
- `docs/spec/ratchet.md`: sections "Flow directory", "Setup", "Split" and "Approvals".
- Anchor: David Parnas.

#### Ratchet step 5. Results contract, check, gate and train

Ports from `ratchet.ts`. The `## oh-my-pi (MIT)` section is already added in step 3.

- `results.go`:
  - `func (f *flow) readVariant(variant string) (variantData, error)`:
    - Stream `results.jsonl` with `bufio.Scanner`; each line may be at most 1 MiB and the file at most 256 MiB.
    - `errors.jsonl` and `traces/` are optional.
    - Each line is decoded to `map[string]jsontext.Value`. Blank lines are skipped.
    - `resultsSHA` is the SHA-256 of `"results\x00" + results bytes + "\x00errors\x00" + errors bytes + "\x00traces\x00" + strings.Join(sorted trace names, "\n")`.
  - `func rowMetrics(row) map[string]float64`:
    - Every top-level JSON number except `rep` becomes a metric.
    - Unless `status` is `truncated`, `grade` true or false gives `score` 1 or 0, a number gives `score`, and an object's number or bool members become metrics under their own keys, overriding row fields.
  - `func meansByCase(rows []resultRow, metric string) caseMeans`: the mean over reps of `metric` for each case id. `readVariant` returns `variantData` with `Rows []resultRow`, and each `resultRow` is `struct{ CaseID string; Rep int; Metrics map[string]float64 }`, built from the decoded line with `rowMetrics`.
- `gate.go`:
  - `func (f *flow) check(s *state, variant string) (checkResult, error)`, in this order:
    1. The variant must match `^(baseline|v[1-9][0-9]{0,3})$`, else the error is `variant must match ^(baseline|v[1-9][0-9]{0,3})$: <name>`.
    2. `done()` must be false.
    3. Every approval must be `current`.
    4. The baseline inputs digest must be unchanged.
    5. The off-limits digest must be unchanged.
    6. The variant must be the expected one: `baseline` with no rounds, otherwise `v<last.Round+1>`, or `last.Variant` when the last decision is `rerun`.
    7. For a non-baseline variant, the change digest must differ from the best round's. For a re-check after a rerun, it must equal the last round's, else the error is `<variant> awaits rerun; restore the change paths to the state gated in the last round`.
    8. Set `Pending` and save.
  - `func (f *flow) gate(s *state, variant, summary string) (gateResult, error)`, in this order:
    1. Replay: when the last round's variant equals `variant` and its `ResultsSHA` equals the current one, return the stored round.
    2. Pending must match the variant, and the change digest must equal `Pending.ChangeSHA`.
    3. Approvals must still be current.
    4. Duplicate check, completeness over `ids × reps 0..reps-1`, and the baseline errored-only refusal. A row with `rep` at or above `reps` is an extra rep. The gate accepts it and averages it with the case's other rows for the same metric, as the original does (`ratchet.ts:745`). The gate reads the reference variant's rows from disk on each call (`ratchet.ts:861`).
    5. Leak checks (D9).
    6. Summaries for the target and each hold metric.
    7. The target must be present, and each guardrail must have n ≥ 2 on both splits and paired.
    8. Baseline warnings: a near ceiling is a higher target whose values all lie in [0,1] with overall mean ≥ 0.95; a mismatch is `|train−test| > hypot(halfs)`. The warning texts are `target <target> is near its ceiling in the baseline` and `baseline <target> train and test differ by more than their intervals`.
    9. Errored-only slots give a rerun. Otherwise call `decideRound(target, pair, guards, best, variant, reruns)`. `best` is the variant of `state.best`, and `reruns` is the `Reruns` of the last round when that round is this variant, else 0. The rerun limit applies inside `decideRound`.
    10. Record the round. The round number is 0 for `baseline`, else the last round's number plus 1. When the last round is this variant with decision `rerun`, the new round replaces it and keeps its number. Otherwise the new round is appended. `Reruns` is the `reruns` value passed to `decideRound`, plus 1 when the decision is `rerun`. Baseline also records `BaselineInputsSHA` and `OffLimitsSHA`.
    11. Call `advance`. Clear `Pending` unless the decision is `rerun`. Set `Best` on `baseline` or `keep`. Save.
  - `func (f *flow) train(s *state, variant string) (trainView, error)`:
    - The variant must be a gated round, else the error is `train needs a gated round for <variant>`.
    - Return the raw rows of train ids only, `traces` paths of train ids relative to the cwd, and `history[case][variant]`, the target mean or null.
  - `func renderTable(s *state) string`:
    - Columns: `round | variant | change | decision | train <target> | test <target>`, then a train and test pair per hold metric.
    - Cells read `mean ±half` with 3 decimals, `–` when there is no mean, and ` (best)` after the best variant.
    - Escape each `|` in the change summary as `\|`.
- `gate_test.go` (written first; it fails to compile until `check` and `gate` exist), using a temp workspace with a split set directly (train `a,b,c,d`, test `e,f,g,h`), reps 1, target `score` higher, and approvals recorded from `digest`:
  - `TestGateClimb`:
    - G1 `baseline`, all scores 0: decision `baseline`; train `{n:4, mean:0, half:0}`; no warnings.
    - G2 `v1`: train a,b,c = 1 and d = 0; test all 1. Train delta is 0.75 ± 0.7955, `flat`. Test is 1 ± 0, `up`. Decision `rerun`, `Reruns` 1, `FlatRounds` 0.
    - G3: append rep 1 rows (v1 all 1, baseline all 0), then gate v1. Train diffs [1,1,1,0.5] give 0.875 ± 0.39775 (s²=0.0625, se=0.125), `up`; test `up`. Decision `keep`, best `v1`.
    - G4 `v2`: train all 1, test all 0. Against v1's means (a,b,c 1; d 0.5; test 1), train diffs [0,0,0,0.5] give 0.125 ± 0.39775, `flat`. Test is −1 ± 0, `down`. Decision `revert` with `score regressed vs v1`, `FlatRounds` 1.
    - Before G4, `check("v2")` changes the change file. G4 also asserts that round `changed_files == ["p.md"]`.
  - `TestGateGuardrail`: hold `latency_s`. Baseline score 0 and latency 1.0 everywhere. v1 score 1 everywhere, train latency [1.2,1.2,1.3,1.3], test latency 1.0. Decision `revert` with `guardrail regressed vs baseline: latency_s`.
  - `TestGateRefusals`. For each row, assert the error text and that `state.json` bytes are unchanged:

| Fixture | Error |
|---|---|
| v1 lacks h rep 0, no error row | `v1 is incomplete: 1 (case, rep) slots have no result or error row (h#0)` |
| a rep 0 twice | `v1/results.jsonl has duplicate (case, rep) rows (a#0); keep one row per slot and gate again` |
| `traces/e_rep0.json` | `v1/traces holds transcripts for test cases (e_rep0.json); write traces for train cases only, delete these, and run again` |
| `traces/notes.txt` | `v1/traces holds a file that is not <case>_rep<k>.json: notes.txt` |
| row e has `"explanation":"x"` | `test row e#0 carries text field explanation; keep output text of test cases out of results.jsonl` |
| baseline `errors.jsonl` a#0 and no a#0 result | `baseline has 1 slots that only errored (a#0); fix the harness and resume the run until every slot is scored` |
| hold `cost_usd`, rows without it | `guardrail cost_usd has fewer than 2 measured cases on train or test in v1; the gate cannot hold a guardrail it cannot measure` |
| rows without `grade` | `no row carries the target metric "score"` |
| change file edited after `check(v1)` | `the change paths changed after check(v1); call check again and run v1 again` |
| gate without check | `call check(v1) before gate(v1)` |
| row without `rep` | `v1/results.jsonl line 3: rep must be a non-negative integer` |
| row with status `done` | `v1/results.jsonl line 1: status must be ok or truncated` |

  - `TestGateErroredOnlyReruns`: v1 with h#0 only in `errors.jsonl` gives `rerun` with `1 slots only errored (h#0); resume v1 until every slot is scored, then gate it again`.
  - `TestGateRerunLimit`: three gates of v1, each with new appended rows that keep train flat and test up, give `rerun`, `rerun`, then `revert` with `rerun limit reached: v1 was rerun 2 times`.
  - `TestGateReplay`: gate v1 twice without changing files. The same decision comes back, `len(Rounds)` is unchanged, and `FlatRounds` is unchanged.
  - `TestCheckRefusals`:

| Fixture | Error |
|---|---|
| grader stale | `approvals are not current (grader: stale); call approve for each stage` |
| v3 when v2 expected | `next variant is v2, got v3` |
| v2 with the same change digest as best | `v2 changes nothing in the change paths since the best round (v1)` |
| cases edited after baseline (re-approved) | `the cases changed after the baseline was gated; start a new flow with init under a new name` |
| off-limits file edited after baseline | `off-limits files changed since the baseline; restore them before the next round` |
| plateau reached | `flow is done: plateau after 3 reverts; change stop with plan and approve the plan again, or start a new flow` |
| v2 while v1 awaits rerun | `next variant is v1, got v2` |

  - `TestTrainHidesTest`: the output JSON contains none of `"e"`, `"f"`, `"g"` or `"h"` as case ids, and its traces list only train files.
- `docs/spec/ratchet.md`: sections "Runner contract", "Gate checks" and "Train view".
- Anchor: Herbert Simon, Edward Tufte.

#### Ratchet step 6. The `ratchet` tool and the approval seam

Ports from `ratchet.ts`. The `## oh-my-pi (MIT)` section is already added in step 3.

- `tool.go`:
  - `const Name = "ratchet"`.
  - `type Option struct{ Label, Description, Preview string }`
  - `type Question struct{ Header, Text string; Options []Option }`
  - `type Answer struct{ Label, Other string }`
  - `var ErrNoClient = errors.New("ratchet approvals need an interactive client")`
  - `var ErrCancelled = errors.New("the question was cancelled")`
  - `type Asker interface { Ask(ctx context.Context, callID string, q Question) (Answer, error) }`. This seam keeps `internal/ratchet` free of the ask package (D1). Step 7 is the only code that maps the ask outcomes onto it.
  - `type Tool struct{ asker Asker }`; `func New(asker Asker) Tool`. Every recorded time, here and in `flow`, is `time.Now().UTC()`.
  - `Spec()` returns a `tool.Spec` with `Name` `ratchet`, the description and `Schema` from the section "Schemas of the ratchet workstream", `Freeform` false and `Keep` `tool.KeepHead`. `Class()` returns `tool.ClassRead`.
  - `Run`:
    - Refuse `!tool.IsLocalRunner(env.Runner)` (`internal/tool/tool.go:57-60`) with `ratchet needs the local runner`.
    - Parse the arguments with `json.Unmarshal(call.Args, &args, json.RejectUnknownMembers(true))`.
    - Refuse members that are not allowed for the action, using the table in the section "Schemas of the ratchet workstream": `<action> does not take <member>`.
    - `var locks sync.Map` holds one `*sync.Mutex` per flow directory. `Run` takes the mutex with `locks.LoadOrStore(dir, new(sync.Mutex))` for every action except `approve`, which locks it itself in its steps 1 and 4 and never holds it while asking. Dispatch uses a `map[string]func(...)`.
    - Marshal with `out, err := json.Marshal(result, json.Deterministic(true))`. On success return `tool.Result{Output: out, OK: true}, nil`. On error return `tool.Result{}, err`. A refusal returns `fmt.Errorf("ratchet %s: %w", action, err)`, which the worker turns into `ok=false` (`internal/agent/tools.go:404`, `(*worker).completion`).
    - When `seed` is absent for `split`, use `sum := sha256.Sum256([]byte(call.ID))` and `binary.BigEndian.Uint64(sum[:8]) >> 11`.
- `approve.go`, `func (t Tool) approve(ctx, callID string, f *flow, a args) (tool.Result, error)`:
  1. Under the lock, load the state and compute the digest. If the approval is already current, return `asked:false`.
  2. Release the lock. The ask limits belong to the ask workstream, Step 1. The adapter in Step 7 checks them, because only that adapter imports the ask package (D1).
  3. Ask `Question{Header: "Ratchet " + stage, Text: question, Options: [{Approve, "Record approval of digest " + sha[:12], hostBlock + "\n\n" + preview}, {Revise, "Record nothing and send feedback in Other"}, {Abort, "Record nothing and stop this flow"}]}`.
  4. Map the outcomes:
     - `ErrNoClient` gives `Result{OK:false, Output:"declined: " + ErrNoClient.Error()}`.
     - `ErrCancelled` gives `Result{OK:false, Output:"cancelled: the question was closed; nothing was recorded"}`.
     - `Approve`: re-lock, reload, recompute. A changed digest gives the error `the <stage> files changed while the user was reading; ask again`. Otherwise record `{sha, at}`, save, and return `approved:true, asked:true`.
     - `Abort` gives `aborted:true`.
     - `Revise`, or an empty label, gives `approved:false` with `feedback` set to `Other`.
     - Any other label gives the error `unexpected answer <label>`.
     - `approve` waits for a person. A cell refuses it with status `denied` (Eval default step 1), so the action runs only as a direct call.
  - `hostBlock` lines:
    - Every stage: `Stage: <stage>`, `Flow: <flow>` and `Digest: <sha>`.
    - inputs and grader: `Files:` followed by `- <path>` lines.
    - plan: `Target: <t> (<dir>)`, `Guardrails: <m> (<dir>), ...` or `none`, `Reps: N`, `Split: N train, M test, seed S`, `Stop: plateau P, rounds R`, `Command: <cmd>`, `Change paths: ...`, `Off limits: ...` or `none`.
- `tool_test.go` (written first; it fails to compile until `Tool` exists) with `fakeAsker{answer Answer; err error; calls int}`:
  - `TestApproveOutcomes`. For each case, assert the output and whether `approvals[stage]` is set:

| Fake reply | Output | Recorded |
|---|---|---|
| `Approve` | `{"approved":true,"asked":true,"sha":"<digest>","stage":"inputs"}` | yes |
| `Revise` + Other `add negatives` | `{"approved":false,"asked":true,"feedback":"add negatives","stage":"inputs"}` | no |
| `Abort` | `{"aborted":true,"approved":false,"asked":true,"stage":"inputs"}` | no |
| `ErrNoClient` | OK false, output is `declined: ratchet approvals need an interactive client` | no |
| `ErrCancelled` | OK false, `cancelled: the question was closed; nothing was recorded` | no |
| already current | `asked:false`, `calls == 0` | unchanged |

  - `TestApprovePlanPreview`: the Approve option preview starts with `Stage: plan\nDigest: ` and contains `Split: 3 train, 1 test, seed 7`.
  - `TestRunRefusesForeignMembers`: `{"action":"gate","flow":"x","variant":"v1","cases":["a"]}` gives `gate does not take cases`.
  - `TestRunNeedsLocalRunner`: `tool.Env{}` gives `ratchet needs the local runner`.
  - `TestSplitSeedFromCallID`: two `split` runs without a seed and with call id `c1` give the same `seed` output.
  - `TestToolFlowEndToEnd`, with a real temp workspace and `runner.NewLocal(t.TempDir())`:
    1. `init`, then `approve` for inputs and grader, then `split` (4+4 strata), then `plan`, then `approve` plan.
    2. `check baseline`; the test writes `results.jsonl` itself; `gate baseline`.
    3. Edit `p.md`, then `check v1`, write rows, `gate v1`. Assert `keep`.
    4. `status` shows all three approvals `current`, and its table holds `v1 (best)`.
- `docs/spec/ratchet.md`: section "Tool", with the schema, per-action members, outputs, the approve outcomes, and the refusal of `approve` in cells.
- Anchor: Saul Bass, Milton Erickson.

#### Ratchet step 7. Battery wiring, top-level threads only

- `internal/agent/thread.go`: after `ID()` (`:354`), add `func (t *Thread) TopLevel() bool { return t.created.Parent == "" && t.created.Advisor == "" }`.
- New file `internal/app/battery/ratchet.go`:
  - `func ratchetTools(ctx context.Context) []tool.Tool`: when `thread, ok := agent.ThreadFromContext(ctx)` succeeds and `thread.TopLevel()` is true, return `[]tool.Tool{ratchet.New(threadAsker{thread})}`; otherwise `nil`.
  - `type threadAsker struct{ thread *agent.Thread }` and its `Ask` method. It is the only code that touches the ask workstream's API (the section "Prerequisites of the ratchet workstream"). It converts the ratchet question to one `asktool.Question` with `MultiSelect` false and calls `thread.Ask`. It maps `OutcomeNoClient` to `ratchet.ErrNoClient`, `OutcomeCancelled` and `OutcomeInterrupted` to `ratchet.ErrCancelled`, `OutcomeFailed` to an error that carries `Detail`, and `OutcomeAnswered` to `ratchet.Answer`. The answer is the first string of `Reply.Answers[0]`, one question in single-select mode. A string equal to an option label becomes `Label`. Any other string becomes `Other`. An empty list gives an empty `Answer`. Before it calls `thread.Ask`, it refuses an empty `Text`, a `Text` over `asktool.MaxQuestion` runes, and an option `Preview` over `asktool.MaxPreview` characters or `asktool.MaxPreviewLines` lines. The error names the limit. The ask workstream, Step 1 owns those constants, and this step declares no copy.
- `battery.go:74`: `Tools: func(ctx context.Context, cwd string) []tool.Tool { return append(projects.tools(ctx, cwd), ratchetTools(ctx)...) }`.
- New test `internal/app/battery/ratchet_internal_test.go`, `TestRatchetToolOnlyOnTopLevelThreads` (written first; it fails until `ratchetTools` exists):
  - `ratchetTools(t.Context())` is empty.
  - With a host from `app.NewHost(..., Batteries: (&Daemon{}).Load)` and `top, _ := host.Start(ctx, agent.ThreadSpec{Cwd: dir})`, `ratchetTools(agent.ContextWithThread(ctx, top))` holds one tool named `ratchet`.
  - A child started with `agent.ThreadSpec{Cwd: dir, Parent: top.ID()}` gets none (`Host.Start` calls `startSession` (`internal/agent/host.go:239`), which calls `sessionCreated` (`:240`), and `sessionCreated` copies `spec.Parent` into the session record, `internal/agent/host.go:257`).
  - `TestThreadAskerMapsDecline` uses the ask workstream's no-client path and asserts `errors.Is(err, ratchet.ErrNoClient)`.
- `docs/spec/daemon.md:45`: append "A top-level dalgona thread MUST get the `ratchet` tool. Child and advisor threads MUST NOT get it."
- `internal/script/mode.go`: add the constant `ratchetName = "ratchet"`, and insert `ratchetName` after `asktool.Name` in `evalFirstNames`. The eval-first direct list then shows `ratchet` to top-level threads. Cells refuse `ratchet` with action `approve`. Eval default step 1 adds that refusal, with the message `tool ratchet approve needs the user and runs only as a direct call`. This step adds no refusal. The literal matches the `Name` of `internal/ratchet`. `internal/script` does not import that package, because dal reaches `internal/script` through `internal/scripthost`, and the ratchet invariant bars dal from importing `internal/ratchet`. Test in `internal/script/eval_test.go`: `TestEvalFirstListsRatchet`, mode `eval-first`, specs `read`, `exec`, `ratchet`, `eval`; expected visible names `[read exec ratchet eval]`. Before the change: `[read exec eval]`.
- Lands after: the ask workstream, Steps 1 and 2 (the `asktool` package and `Thread.Ask`), Ask step 4 (the `asktool.Name` entry in `evalFirstNames`), and Step 6 of this workstream.
- Anchor: Christopher Alexander.

#### Ratchet step 8. The `ratchet` keyword

- In `builtinTable`, insert after `orchestrate`: `{Word: "ratchet", Hue: [2]int{20, 70}, Requires: []string{"ratchet", "exec", "task"}, Effort: "default", Notice: notice("ratchet"), Enabled: product == "dalgona"}`.
- New file `internal/keywords/notices/ratchet.md`, exactly:

```
Improve one LLM flow with a ratchet: build a trusted eval, then try one change per round and keep it only when it wins on held-out cases. The `ratchet` tool owns approvals, the frozen split, the completeness checks and the keep or revert decision. Never edit files under `.dalgona/ratchet/` yourself.

Build the eval:
1. Read the code that runs the flow and find its real entry point.
2. Gather 15 to 100 cases with expected answers. Include cases that should trigger the behavior and cases that should not. Note where each expected answer came from.
3. Call `ratchet` with action `init`: case files, harness files (runner and grader), the files you may change, and the files that are off limits.
4. Call action `approve` with stage `inputs`. Put the case ids in the preview, one line each. The preview holds at most 40 lines, so group ids by tag when the set is large. Revise until the user approves.
5. Write a runner that calls the real entry point. It appends one line per case and rep to `results.jsonl` and puts failed attempts in `errors.jsonl`, never a zero score. It writes traces for train cases only and keeps output text out of held-out rows.
6. Choose the cheapest grader that measures the property. A judge model must differ from the model under test.
7. Pilot 3 to 5 cases and read one full row. Then call `approve` with stage `grader` and show five graded pilot cases.

Plan and baseline:
8. Call `split`, then `plan` with the target, guardrails, reps, stop rule and runner command.
9. Call `approve` with stage `plan`. This is the last question for the user.
10. Call `check` for `baseline`, run its command with `exec`, then call `gate` for `baseline`. Read every warning.

Climb, one change per round:
11. Call `train` for the best variant. Give its rows and trace paths to one analyzer, called from an `eval` cell as `tools.task(...)`, because `task` is not a direct tool in `eval-first`. Ask for the behavior that costs the most, quoted evidence and one diff.
12. Apply that one change to the change paths only. Never copy case text into them.
13. Call `check` for the next variant, run the command, then call `gate` with a one-line summary.
14. Follow the decision. On `keep`, build on it. On `revert`, undo the change before the next round; `check` lists files that still differ from the best round. On `rerun`, resume missing slots or add reps to this variant and the best one, then gate again.
15. Report one line per round: decision, test score with its interval, and best so far.
16. When `plateau` is true, sort the remaining train failures by cause before another change. When `done` is true, stop climbing.

Report:
17. Leave the change paths at the best round. Report the test change against the baseline with its interval. Say so plainly when the gain is within noise.

If `approve` declines because no client is attached, stop and say that an interactive session is needed.
```

- Tests:
  - `TestBuiltinsProductMetadata`: dalgona order becomes ultrathink, jevify, workflowz, orchestrate, ratchet, decide, simplify; dal is unchanged from step 2.
  - `TestBuiltinsNotices`: add `"ratchet": {"ratchet", "approve", "gate", "exec", "task", "held-out"}`.
  - `TestDefaultEnabled`: add `{"dal","ratchet",false}` and `{"dalgona","ratchet",true}`.
  - The guard test from step 1 fails until the file exists. The other new assertions in this step fail before the change, because the table has no `ratchet` row yet.
- `docs/spec/keywords.md`:
  - Insert the row between the `orchestrate` and `decide` rows: `` `ratchet` | `[20, 70]` | `ratchet`, `exec`, `task` | `default` ``.
  - Insert the contract between the `orchestrate` and `decide` bullets: "`ratchet`: build the eval through the `ratchet` actions in order, get each approval from the user, run the runner with `exec`, give the analyzer only the train view through `tools.task(...)` in an `eval` cell, follow each gate decision, and stop at `done`."
- Lands after: Steps 1 and 2 of this workstream (the guard test, and the `decide` and `simplify` rows and contracts that Step 2 adds).
- Anchor: J.R.R. Tolkien, Italo Calvino.

#### Schemas of the ratchet workstream

Config keys: none. Journal events: none; the tool output is the record. Wire: none new. `approve` uses the ask workstream's `item/tool/requestUserInput` with one question, `isBlocking: true`.

##### Tool spec

Description, exact text:

```
Run a ratchet flow: a held-out eval that keeps a change only when it wins on both train and test cases. State lives in .dalgona/ratchet/<flow>/. Actions: init sets case, harness, change and off-limits paths; plan sets the target, guardrails, reps, stop rule and runner command; split assigns case ids to train and test by stratum tag; approve asks the user to approve inputs, grader or plan and binds a content hash; check preflights a variant and returns its runner command; gate judges a finished run as keep, revert or rerun; train returns train rows, train traces and score history for the analyzer; status returns approvals and the round table. The result is JSON text.
```

Schema, exact text:

```json
{"type":"object","properties":{"action":{"type":"string","enum":["init","plan","split","approve","check","gate","train","status"]},"flow":{"type":"string"},"cases":{"type":"array","items":{"type":"string"}},"harness":{"type":"array","items":{"type":"string"}},"change":{"type":"array","items":{"type":"string"}},"off_limits":{"type":"array","items":{"type":"string"}},"target":{"type":"string"},"direction":{"type":"string","enum":["higher","lower"]},"hold":{"type":"array","items":{"type":"string"}},"directions":{"type":"object","additionalProperties":{"type":"string","enum":["higher","lower"]}},"reps":{"type":"integer"},"plateau":{"type":"integer"},"rounds":{"type":"integer"},"command":{"type":"string"},"strata":{"type":"object","additionalProperties":{"type":"string"}},"test_fraction":{"type":"number"},"seed":{"type":"integer"},"stage":{"type":"string","enum":["inputs","grader","plan"]},"question":{"type":"string"},"preview":{"type":"string"},"variant":{"type":"string"},"summary":{"type":"string"}},"required":["action","flow"],"additionalProperties":false}
```

##### Members by action

`action` and `flow` are always allowed.

| Action | Required | Optional | Output (keys sorted) |
|---|---|---|---|
| init | cases, harness, change | off_limits | `{"cases":[..],"change":[..],"dir":".dalgona/ratchet/x","flow":"x","harness":[..],"off_limits":[..]}` |
| plan | none | target, direction, hold, directions, reps, plateau, rounds, command | `{"command":"..","goal":{..},"reps":2,"stop":{"plateau":3,"rounds":10}}` |
| split | strata | test_fraction (default 0.4), seed (0 to 2^53−1) | `{"seed":7,"strata":{"pos":{"test":2,"train":3}},"test":3,"train":5}` |
| approve | stage, question | preview | see the step 6 table |
| check | variant | none | `{"changed_files":["p.md"],"command":"..","expected_rows":8,"out_dir":".dalgona/ratchet/x/v1","reps":1,"variant":"v1"}` |
| gate | variant | summary (required unless the variant is `baseline`; at most 200 runes) | `{"best":{..},"changed_files":[..],"decision":"keep","deltas":{"score":{"test":{..},"train":{..}}},"done":false,"done_reason":"","flat_rounds":0,"plateau":false,"reasons":[..],"table":"..","test":{..},"train":{..},"variant":"v1","warnings":[..]}` |
| train | variant | none | `{"history":{"a":{"baseline":0,"v1":1}},"rows":[{..}],"traces":[".dalgona/ratchet/x/v1/traces/a_rep0.json"],"variant":"v1"}` |
| status | none | none | `{"approvals":{"grader":"current","inputs":"current","plan":"stale"},"best":{..},"done":false,"done_reason":"","flat_rounds":0,"flow":"x","plateau":false,"table":".."}` |

##### `state.json` example

All fields are snake_case. Means and halves are `null` when absent.

```json
{"flow":"inbox-routing","cases_paths":["eval/cases.jsonl"],"harness_paths":["eval/run.py"],"change_paths":["prompts/router.md"],"off_limits":[],
 "goal":{"target":"score","direction":"higher","hold":["latency_s"],"directions":{"latency_s":"lower"}},
 "reps":2,"stop":{"plateau":3,"rounds":10},"command":"python eval/run.py --variant {variant} --out {flow_dir}/{variant}",
 "train_ids":["a","b"],"test_ids":["c","d"],"split_seed":7,"test_fraction":0.4,
 "approvals":{"inputs":{"sha":"<64 hex>","at":"2026-11-11T07:00:00Z"}},
 "baseline_inputs_sha":"<64 hex>","off_limits_sha":"<64 hex>",
 "pending":{"variant":"v2","change_sha":"<64 hex>","at":"..."},
 "rounds":[{"round":1,"variant":"v1","change":"tighten the refund rule","decision":"keep","reasons":[".."],"warnings":[],
   "train":{"score":{"n":4,"mean":0.875,"half":0.39775}},"test":{"score":{"n":4,"mean":1,"half":0}},
   "deltas":{"score":{"train":{"n":4,"mean":0.875,"half":0.39775,"verdict":"up"},"test":{"n":4,"mean":1,"half":0,"verdict":"up"}}},
   "errors":0,"truncated":0,"reruns":1,"results_sha":"..","change_sha":"..","change_files":[{"path":"prompts/router.md","sha":".."}],"changed_files":["prompts/router.md"],"at":".."}],
 "best":{"round":1,"variant":"v1","test_score":1},"flat_rounds":0}
```

##### Runner files under `<flow_dir>/<variant>/`

- `results.jsonl` row. Required: `case_id` (string), `rep` (integer ≥ 0), `grade` (bool, number, or object of numbers and bools), `status` (`ok` or `truncated`). Optional: `tags` (strings), `model` (string), and any numeric field such as `latency_s` or `cost_usd`. Train rows may also carry text such as `prompt`, `output` or `explanation`; test rows may not.

  Example: `{"case_id":"a","rep":0,"tags":["refund"],"grade":{"pass":true,"format":1},"status":"ok","model":"local/qwen-test","latency_s":1.4}`

- `errors.jsonl` row. Required: `case_id`, `rep`, `class` (non-empty string). Optional: `message`.

  Example: `{"case_id":"h","rep":0,"class":"timeout","message":"no reply after 60 s"}`

- `traces/<case_id>_rep<k>.json`: train case ids only.

- Placeholders in `command`: `{variant}` (required) and `{flow_dir}` (`.dalgona/ratchet/<flow>`).

##### Keyword table after this workstream

| Word | Hue | Requires | Effort | Products |
|---|---|---|---|---|
| ultrathink | [0, 330] | none | max | both |
| jevify | [300, 420] | eval | default | dalgona |
| workflowz | [30, 150] | eval, task | default | dalgona |
| orchestrate | [150, 280] | task | default | dalgona |
| ratchet | [20, 70] | ratchet, exec, task | default | dalgona |
| decide | [200, 250] | none | default | both |
| simplify | [90, 140] | none | default | both |

##### Edge cases the worker must handle

- Symlinks under hashed paths are opened through `os.Root`, so a link that escapes the cwd is an error.
- Rows for ids outside the split are ignored with the warning `<n> result ids are outside the split and were ignored`.
- Truncated rows count toward completeness but not toward quality means; this gives the warning `<n> truncated rows excluded from quality means`.
- Error rows give the warning `<n> infra error rows recorded in errors.jsonl`.
- `errors.jsonl` may repeat a slot across resumes; a result row for a slot supersedes its error rows.
- `state.json` is not tamper-proof. The spec says so: the gate guards against mistakes, not a hostile model.
- Case inputs in the case files stay readable by the analyzer. Held-out isolation covers outputs and traces, as in the original. Test grades in `results.jsonl` stay readable by `read` in the same thread, and the spec must state that limit.

#### Checks of the ratchet workstream

1. `go test -race -count=1 ./internal/keywords/...` (steps 1, 2, 8).
2. `go test -race -count=1 ./internal/ratchet/...` (steps 3 to 6). `TestToolFlowEndToEnd` shows the new behaviour end to end through the tool, from `init` to `keep`.
3. `go test -race -count=1 ./internal/app/battery/... ./internal/agent/...` (step 7).
4. `go tool golangci-lint run ./...`. Depguard must reject an import of `internal/ratchet` from `cmd/dal`; check this once by hand on a scratch edit and then revert it.
5. `just verify` before every commit.
6. Live check, once the ask workstream, Steps 1, 2, 3 and 3a land and the scripted local server is up:
   1. Start `just dalgona` against the scripted local chat-completions server (`internal/llmstub`, the live-debug workstream, Step 1 and 2).
   2. Prompt `ratchet the greeting prompt in ./fixture`.
   3. Confirm the notice row appears and that `approve` shows a question with `Approve`, `Revise` and `Abort` in the TUI.
   4. Detach all clients and repeat `approve`. The output must be `declined: ratchet approvals need an interactive client`.

#### Prerequisites of the ratchet workstream

- The ask workstream, Steps 1 and 2 must land before step 7. Its interface is cross-phase decision 12, and `asktool` is the import alias of `github.com/gosuda/dalgona/internal/tool/ask`. Routing with two clients attached follows the question rule of the ask workstream, Step 3 (cross-phase decision 6). Its Step 3a applies the same rule to approvals. `RequestUserInput` is absent from `internal/agent` and `internal/tui`; in `internal/` it appears only under `internal/rpc/testdata/codex-schema/`, so the ask workstream is not in the tree yet. `threadAsker` calls:

  ```go
  func (t *Thread) Ask(ctx context.Context, callID string, questions []asktool.Question) (asktool.Reply, error)
  ```

  with:
  - `asktool.Question` (ask workstream, Step 1)
  - `asktool.Option` (ask workstream, Step 1)
  - `asktool.Reply` and `asktool.Outcome` (ask workstream, Step 1)
  - the outcome mapping, which only Step 7 of this workstream holds

  `internal/app/battery/ratchet.go` is the only file that calls the ask interface.
- Step 3 lands after the edit-review workstream, Step 1, because both edit `.golangci.yml`. The live check in the section "Checks of the ratchet workstream" needs the ask workstream, Steps 3 and 3a, and the live-debug workstream, Steps 1 and 2. Shared files: `docs/spec/keywords.md` (the protocol workstream, Step 7 owns the Spans and Usable text; Steps 1, 2 and 8 of this workstream add only the sentence and rows named in them), `.golangci.yml` (Step 3 owns its two deny entries), and `justfile` (Step 3 owns the `dist` recipe; the live-debug workstream, Step 3 owns its recipes). Apart from these shared edits, Steps 1, 2, 4, 5, 6 and 8 need no other workstream. Within this workstream, steps run in numeric order, and Step 8 follows Step 2.

