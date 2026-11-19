### Workstream: Code rewrite

Agents change code by structure with the `rewrite` tool. It runs a tree-sitter query over the files under `path` that match `glob`, and it replaces the text of one named capture in each match. The tool is edit class, so every call gets the approval, the stale-file guard, the pre-image journal, the rollback and the edit review that `patch` gets. Planning runs in the host on a local runner under fixed bounds: 1 MiB per file, 256 candidate files, 32 changed files, 256 replacements and 90 seconds. Apply runs after approval under the caller's context and has no rewrite deadline. The tool writes nothing when a file would break, when a bound is exceeded, when a target is empty or when two targets overlap. The read-only preview is the `syntax` mode of `search` with the same `lang`, `pattern`, `path` and `glob`. Both products get the tool through `toolSet`. Each step is one commit that builds and passes `just verify`.

#### Decisions of the Code rewrite workstream

1. A new edit-class tool named `rewrite`, beside `patch`. Choice: `tool.Editor` with `tool.ClassEdit`, registered in `toolSet`. Rival: a `search` mode that writes, or a payload form of `patch`. Reason: approval (`internal/agent/gate.go:130-131`), the stale guard, the journal and the edit review key on the class. `search` is `tool.ClassRead` (`internal/tool/search/search.go:34`) and runs in a child process (`search.go:49`).
2. Planning runs in the host, on a local runner only. Choice: `Plan` walks the workspace with `search.Select` and reads through `env.Runner` after `tool.IsLocalRunner` passes. Rival: parse in the search child. Reason: `Plan` must return pre-images and digests to the host (`FileChange.Pre` and `PreSHA` in `internal/tool/tool.go`), and the child returns text rows only.
3. One candidate function for every mode. Choice: `search.Select` (Code rewrite step 2) is the exported form of `parseCandidates` (Code symbols step 6). The `symbols`, `definition` and `syntax` modes call `parseCandidates`, and this tool calls `Select`, so one selection serves every caller. Rival: a second walker in the rewrite package. Reason: one ignore set, one symlink rule (`internal/tool/search/engine.go:153`) and one glob rule (`matchingFiles`, `engine.go:273-276`) for every mode.
4. The rewrite tool parses in the host. Choice: `rewrite` parses in the host under the bounds of decision 6. The Code symbols rule that parsing runs only in the search child covers the modes `symbols`, `definition` and `syntax`. The decision record of the later reviews says so. Rival: run the rewrite parses in a child process. Reason: `Plan` must hand pre-images to the host, and a child would have to return up to 32 MiB of file text through a row protocol that carries text rows only.
5. Writes go through a new `patch.PlanReplace`. Choice: the rewrite computes each post-image in memory and passes byte ranges to `patch`. Rival 1: copy the staging and writer code into the rewrite package. Rival 2: express each replacement as a line edit of the public patch model. Reason: the public patch model locates lines, while byte splices exist only inside `internal/tool/patch` (`resolve.go:85`, `stage.go:38`). A copy would fork the stale guard and the rollback.
6. The planning re-read has its own message, and the apply guard keeps the existing one. Choice: planning returns `patch: <path> changed while the rewrite was planned. Nothing was written. Try again`. Apply keeps `verifySource`, whose message is `patch: <path> changed while you were approving. Nothing was written. Try again` (`internal/tool/patch/writer.go:81`). Rival: one message for both. Reason: the planning message tells the caller which step found the change.
7. Bounds are constants of the rewrite package, and each bound refuses the call. Choice: `rewriteMaxFileBytes = 1 << 20`, `rewriteMaxScanned = 256`, `rewriteMaxFiles = 32`, `rewriteMaxEdits = 256` and `rewriteDeadline = 90 * time.Second`. There is no separate byte total. Planning reads one file at a time, keeps only changed files, and `patch.PlanReplace` applies the existing 64 MiB stage cap (`stage.go:88`). Rival 1: skip an oversize file with a footer, as `search` does. Rival 2: add a total byte cap equal to the syntax preview's 1 MiB per call. Reason for Rival 1 rejected: a skipped file leaves its matches unchanged while the call reports success. Reason for Rival 2 rejected: the owner's bound list names no total, and the deadline and the per-file cap bound the read.
8. The file and replacement limits count what would change. Choice: `rewriteMaxFiles` counts files whose post-image differs from the pre-image, and `rewriteMaxEdits` counts the edits of those files. Rival: count matches. Reason: the owner set the limits on files and replacements that would change.
9. A missing capture, a repeated capture name and a zero-width target refuse the call. Choice: `rewrite: @<name> is not in the match at line <N>`, `rewrite: @<name> occurs twice in one match at line <N>` and `rewrite: target <name> has zero width at line <N>`. Rival: insert empty text, or the first capture wins. Reason: an empty insertion silently corrupts code, and two candidates for one target make the edit ambiguous.
10. Parse count. Choice: one parse of each candidate, then one parse of the post-image of each changed file, because the post-image must not gain syntax errors. Rival: skip the post-image check. Reason: the owner's rule that the call writes nothing when a file would break needs the check. The correction to the earlier one-parse decision is accepted.
11. A candidate file with syntax errors before the call refuses the call. Choice: `rewrite: <path> has syntax errors; fix it with patch first`, for any candidate, with or without a match in it. Rival: skip the file when it has no match. Reason: the owner's ruling is categorical, and the message names the repair path.
12. The replacement grammar. Choice: `@@` is one literal `@`. `@name` is a capture reference, where a name is one or more ASCII letters, digits or `_`. `@{name}` is a capture reference for a name that may contain dots, such as `@{definition.function}`. Any other `@` refuses the call with `rewrite: replace has a lone @ at byte <N>; write @@ for a literal @`. Rival: a dot that joins name parts, as in `@name.y`. Reason: a dot after a capture would silently name another capture (`@name.String()` names `name.String`). The braces form keeps every dotted capture name reachable and leaves the text after a short reference unchanged.
13. The result text is a rewrite summary, then the tagged post-images. Choice: `PlanReplace` takes the summary and writes it in place of the line `patch: applied N file(s)`. Rival: keep the patch header. Reason: the caller needs the replacement count of each file, and a header above the summary hides nothing but adds a second count.
14. Masking follows `patch` for locator text and new content only. Choice: `pattern` and `replace` pass through `env.Masker.UnmaskStrict`. `path`, `glob`, `lang` and `target` do not. Rival: unmask `replace` only. Reason: `docs/spec/edit-styles.md:82` allows unmasking of locator text and new content, and forbids unmasking of paths.
15. Registration beside `patch`. Choice: `rewrite.New()` follows `patch.New(cfg.EditStyle.EditStyle(modelID))` in the tool list at `internal/app/app.go:150`, and `toolSet` is reached by both products through `agent.NewHost` (`app.go:89`). Rival: a battery package. Reason: `toolSet` is the one list of edit tools.
16. The eval visible list does not change. Choice: `rewrite` is not added to `evalFirstNames` (`internal/script/mode.go:13`). It folds under `eval-first` (`FoldedTools`, `mode.go:41-54`) and reaches cells after Eval default step 1 (`inCells`, which excludes only `eval` and `yield`). Rival: add `rewrite` to the visible list. Reason: the assumptions of this plan say that the rewrite tool adds no name to the direct list.
17. The item kind comes from the class. Choice: no change to `toolKindByName`. `toolKind` maps class `edit` to `kindFileChange` (`internal/rpc/serve_items.go:147-148`), and `toolKindByName` (`serve_items.go:161`) runs only when the class is empty. A `rewrite` change to `toolKindByName` would be dead code. Rival: add `rewrite` to `toolKindByName`. Reason: the class already decides the kind.
18. The edit review already names rewrite. Choice: the edit-review workstream needs no change. Its outcome names the edit unit as a call of an edit-class tool (`patch`, `apply_patch` or `rewrite`), and its judge state lists `rewrite` as a value of `tool`. Rival: exclude `rewrite`. Reason: the owner asked that a rewrite be judged like a patch.
19. No read-only sandbox exists at this commit, so the rewrite has no sandbox check of its own. Choice: `rewrite` follows `patch`, and `patch` has no sandbox check. `runner.SandboxMode` has two values (`internal/runner/runner.go:9-13`), and the wire value `read-only` maps to `config.SandboxWorkspaceWrite` (`internal/rpc/serve_thread.go:72-73`). A future read-only mode adds one check to `patch` and to `rewrite` in the same commit. Rival: invent a refusal path. Reason: the owner's rule says a read-only call is refused by the path that `patch` uses, and that path refuses nothing at this commit. The owner must confirm this premise (see not_done).
20. Line endings are spliced as written. Choice: the replacement bytes go into the post-image as the call wrote them. The tool does not convert LF to CRLF or CRLF to LF. The tool description and `docs/spec/rewrite.md` say so. Rival: convert each LF in a CRLF file to CRLF. Reason: the owner ruled that the tool does not convert line ends.
21. Oversize files and the preview. Choice: a candidate over 1 MiB refuses the call with `rewrite: <path> is over 1048576 bytes; narrow path or glob.`. A file outside the selection never refuses. The `syntax` preview lists a file over its own file cap in its footer as `[Not parsed: N files. Narrow path or glob.]` (Code symbols constants, `parseMaxFileBytes` 256 KiB). The two caps differ, and cross-phase decision 4 records the gap for the parent. Rival: skip the oversize file in the rewrite. Reason: the owner set the refusal, and a skip would report success.
22. Symbolic links. Choice: `walkScope` drops symbolic links (`internal/tool/search/engine.go:153`), so a link is neither a candidate nor a refusal and is never journaled.
23. No match is a success. Choice: when no candidate matches, or when no candidate changes, the call returns a successful result whose text is `No matches for <pattern>`, with no file in the plan. Rival: an error. Reason: `search` returns the same text as a normal outcome (`internal/tool/search/engine.go:198`).
24. The planning order of each file matches `stageGroups`. Choice: resolve the path, read the target, then pin its directories with `pinVerifiedComponents`. `stageGroups` reads in `stageFile` (`internal/tool/patch/stage.go:79`) and pins after the read (`stage.go:93`). Rival: pin first. Reason: the same order gives the same refusal on a racing change.
25. Select takes a context, and the walk is not interruptible. Choice: `search.Select(ctx, cwd, path, glob, exts)` returns `ctx.Err()` when the context ended during the walk. The walk itself is not interrupted, because `walkScope` takes no context. The deadline also stops every later read and parse. Rival: give `walkScope` a context. Reason: that changes `find` and `grep`, which are outside this workstream.
26. Apply has no rewrite deadline. Choice: planning runs under `context.WithTimeoutCause(ctx, rewriteDeadline, errRewriteDeadline)`. Apply runs under the caller's context, after approval (`gateCall` runs before the approval, `internal/agent/tools.go:272`). Rival: a deadline on apply. Reason: a human may take minutes to approve, and the writer already rolls back under `context.WithoutCancel` (`internal/tool/patch/writer.go:50-53`).
27. A replacement that still matches runs again on each call. Choice: the call is allowed. The tool description and the spec say that a second call changes the file again. Rival: refuse a replacement that matches the query. Reason: the owner ruled that such a call is allowed and documented.
28. The approval view. Choice: in `ask` mode the user sees, in this order: an `item/fileChange/patchUpdated` notification with one change per planned file, each with its unified diff and its counts (Protocol step 5 builds it from the planned pre-image and post-image for every `tool.ClassEdit` call, and `serve_approval.go:22` runs `ask`), then an `item/fileChange/requestApproval` whose reason is `rewrite <path>, <path>` (`internal/agent/gate.go:70-75` joins the planned paths, and `internal/agent/tools.go:278` fills them from `plan.Files`). The pattern and the replacement text are not shown as text. Their effect is shown in the diff. Rival: add the pattern to the reason. Reason: the diff shows every changed line, and a second text field would repeat it.
29. Step 6 has no production code. Choice: the step adds tests only. Each test fails before step 5, because the tool is absent. Rival: add `rewrite` to `toolKindByName`. Reason: decision 17.
30. The harness takes extra tools. Choice: `harness` (`internal/agent/harness_test.go:272`) gets a field `extraTools []tool.Tool`, and its `tools` method appends those tools after the four fixed ones. Rival: a second harness. Reason: one harness serves all agent tests.

#### Code rewrite step 1. Syntax additions for edits

Anchor: David Parnas

- Files: `internal/syntax/symbols.go` (the `Report` type and the `Parser.Symbols` method of Code symbols step 1), `internal/syntax/query.go` (the `Parser.Captures` method of Code symbols step 8), `internal/syntax/language.go` (the `language` struct of Code symbols step 1), `internal/syntax/syntax_test.go`.
- Change:
  - `Report` gains the field `ErrorLine int`. It is the 1-based row of the first node that the library reports as `IsError` or `IsMissing`, and it is 0 when no such node exists or when the parse stopped. the shared `parse` of Code symbols step 1 sets it through one helper `func firstErrorLine(root *gotreesitter.Node) int`. The helper visits a node only when `HasError()` is true, returns at once for a node where `IsError()` or `IsMissing()` is true, and returns `StartPoint().Row + 1` of that node. Library facts, module `github.com/odvcencio/gotreesitter@v0.57.0`: `(*Node).IsMissing` at `tree.go:2248`, `(*Node).IsError` at `tree.go:2252`, `(*Node).HasError` at `tree.go:2256`, `(*Node).StartPoint` at `tree.go:2311` (its `Row` field is the library `Point` row), `(*Node).ChildCount` at `tree.go:2352` and `(*Node).Child` at `tree.go:2355`.
  - `func (q *Query) CaptureNames() []string` returns the distinct capture names of the query, sorted with `slices.Sort`. It calls the library method `(*Query).CaptureNames` (`github.com/odvcencio/gotreesitter@v0.57.0/query.go:1163`), which returns `slices.Clone(q.captures)` (`query.go:1164`). The sort is added here.
  - `func (l Language) Extensions() []string` returns a copy of the extension list of the language.
  - Names added here are the only exported additions of this step. Code rewrite steps 4 and 5 call them.
- Tests that fail before the change, all in `internal/syntax/syntax_test.go`:
  - `TestCaptureNamesSortedDistinct`. Input: `ByName("go")` compiled with the pattern `(function_declaration name: (identifier) @name) @definition.function`. Expected: `["definition.function", "name"]`. Source: the capture names of the pattern, sorted.
  - `TestErrorLineOfBrokenGo`. Input: the Go source `package a\n\nfunc Bad( {\n` with the pattern `(function_declaration name: (identifier) @name)`. Expected: `Report.SyntaxErrors` is true and `Report.ErrorLine` is 3. Source: the broken text starts on line 3 of the input.
  - `TestErrorLineZeroOnValidGo`. Input: `package a\n\nfunc Good() {}\n`. Expected: `SyntaxErrors` is false and `ErrorLine` is 0. Source: the input is valid Go.
  - `TestExtensionsOfGo`. Input: `ByName("go")`. Expected: `[".go"]`. Source: the `go` row of the extension table in Code symbols Schemas.
- Specification text: none. This step adds no user-visible behaviour.
- Lands after: Code symbols step 8.

#### Code rewrite step 2. Shared candidate selection

Anchor: Herbert Simon

- Files: new `internal/tool/search/select.go`, new `internal/tool/search/select_test.go`, `internal/tool/search/parse.go` (the candidate listing that Code symbols step 6 creates; its three parse modes of Code symbols steps 6 to 8 use it).
- Change: `func Select(ctx context.Context, cwd, path, glob string, exts []string) ([]string, error)`. Its steps, in order:
  1. `scope, err := resolveScope(cwd, path)` (`internal/tool/search/engine.go:54`). Its error text `search: path must stay within the workspace` (`engine.go:59`) is returned unchanged.
  2. `entries, err := walkScope(cwd, scope)` (`engine.go:102`). The walk drops symbolic links and `.git` (`engine.go:153`), and it sorts by display path.
  3. Call `parseCandidates(ctx, entries, glob, accept)` (Code symbols step 6, `parse.go`). `accept` reports whether the lower-cased `filepath.Ext(display)` is one of `exts`. The entries of `exts` are dotted, such as `.go`. `parseCandidates` applies the glob, which matches a base name at any depth when it has no `/` (`internal/tool/search/search.go:28`), and it returns `ctx.Err()` when the context has ended.
  4. Return the display paths of the entries that `parseCandidates` keeps, in order.
  `Select` and the parse modes share `parseCandidates`. The walk of steps 1 and 2 stays in `Select`.
- Tests that fail before the change, in `internal/tool/search/select_test.go`. Each test builds its tree in `t.TempDir()`:
  - `TestSelectByExtensionAndPath`. Files `a/x.go`, `a/y.ts` and `b/z.go`. Call `Select(ctx, root, "a", "", []string{".go"})`. Expected: `["a/x.go"]`. Source: step 3 of this step.
  - `TestSelectGlobMatchesBaseNameAtAnyDepth`. Same tree. Call `Select(ctx, root, ".", "z.go", []string{".go"})`. Expected: `["b/z.go"]`. Source: `search.go:28`.
  - `TestSelectRefusesPathOutsideWorkspace`. Call `Select(ctx, root, "../x", "", []string{".go"})`. Expected error text: `search: path must stay within the workspace`. Source: `engine.go:59`.
  - `TestSelectSkipsSymbolicLinks`. Files `a/x.go`, and a symbolic link `link.go` that points to `a/x.go`, both under root. Call `Select(ctx, root, ".", "", []string{".go"})`. Expected: `["a/x.go"]`. Source: `engine.go:153`. The test skips on Windows, where creating a link needs a privilege the test does not have.
  - `TestSelectReturnsContextError`. Input: a context that is already cancelled, and the tree of `TestSelectByExtensionAndPath`. Expected: `errors.Is(err, context.Canceled)`. Source: the context check of `parseCandidates` (Code symbols step 6).
- Tests that keep passing: `TestSymbolsOutlineOfTree` and `TestSymbolsNotParsedFooter` of Code symbols step 6.
- Specification text: none. The `search` table in `docs/spec/search.md` stays as written.
- Lands after: Code symbols step 8.

#### Code rewrite step 3. Replacement entry in patch

Anchor: Tony Hoare

- Files: new `internal/tool/patch/replace_plan.go`, new `internal/tool/patch/replace_plan_test.go`, `internal/tool/patch/stage.go` (the struct `stagedBatch` at `stage.go:42` gets the field `summary string`, and the 64 MiB check of `stageGroups` at `stage.go:88-91` moves into a helper that both functions call), `internal/tool/patch/writer.go` (`appliedOutput` at `writer.go:440` takes `summary string` as its first argument, and the one call in `applyBatch` at `writer.go:64` passes `batch.summary`).
- Change:
  - `type ReplacedFile struct { Path string; Before []byte; After []byte; Splices []snapshot.Splice }`. `Path` is the display path that `search.Select` returns.
  - `func PlanReplace(ctx context.Context, env tool.Env, summary string, files []ReplacedFile) (tool.Plan, error)`. For each entry, in order:
    1. `display, absolute, err := resolveEnvPath(ctx, env, f.Path)`.
    2. `current, info, err := readTarget(ctx, env, absolute, display)`. The read comes before the pin, as in `stageFile` and `stageGroups` (`stage.go:79`, `stage.go:93`).
    3. When `bytes.Equal(current, f.Before)` is false, return `patch: <path> changed while the rewrite was planned. Nothing was written. Try again`.
    4. When `bytes.Equal(f.Before, f.After)` is true, skip the entry.
    5. Build `&stagedFile{path: display, absolute: absolute, before: current, after: f.After, exists: true, mode: info.mode, splices: f.Splices, sound: true}`.
    6. `file.pin, err = pinVerifiedComponents(env, display, absolute)`. On an error, close every pin that is open and return the error.
    7. Add the file through the helper of this step, which returns the error text of `stageGroups` when the total exceeds 64 MiB.
  - The plan is `tool.Plan{Files: batch.changes(), Apply: func(ctx context.Context) (tool.Result, error) { return applyBatch(ctx, env, batch) }, Discard: batch.closePins}` with `batch := stagedBatch{style: Replace, files: files, summary: summary}`. `Replace` is the constant at `internal/tool/patch/ir.go:9`. The caller passes changed files only.
  - `func appliedOutput(summary string, files []*stagedFile, refs map[string]string) []byte`. When `summary` is empty, the output starts with `patch: applied N file(s)` as before. Otherwise it starts with `summary`, written as it is. The file sections that follow do not change.
- Tests that fail before the change, in `internal/tool/patch/replace_plan_test.go`. They use `editTestEnv` (`property_test.go:22-30`) and `writeFixture` (`property_test.go:33`):
  - `TestPlanReplaceRefusesChangedSource`. File `a.txt` holds `two\n`. Entry: `Path` `a.txt`, `Before` `one\n`, `After` `uno\n`. Expected: `PlanReplace` returns the error text `patch: a.txt changed while the rewrite was planned. Nothing was written. Try again`, and `a.txt` still holds `two\n`. Source: the message of this step.
  - `TestPlanReplaceApplyWritesAndSummarises`. File `a.txt` holds `one\ntwo\n`. Entry: `Before` `one\ntwo\n`, `After` `uno\ntwo\n`, `Splices` `[{Start: 0, End: 3, Inserted: 3}]`. Summary `rewrite: 1 replacements in 1 files\na.txt: 1 replacements\n`. Expected: the output of `Apply` starts with the summary, then `[a.txt#` and the 8-hex value of `snapshot.Tag("whole", After)` (`internal/tool/snapshot/tag.go:9-14`), then `]`, then the line `1:uno`. The file holds `uno\ntwo\n`. Source: `appliedOutput` and `snapshot.Tag`.
  - `TestPlanReplaceKeepsPreImage`. Same entry as the previous test. Expected: `Plan.Files[0].Pre` equals `one\ntwo\n`, and `Plan.Files[0].PreSHA` equals `snapshot.Digest([]byte("one\ntwo\n"))` (`internal/tool/snapshot/store.go:423`). Source: `Pre` must hold the exact pre-image bytes, as `docs/spec/edit-styles.md:78` says.
- Specification text: none in this step.
- Omission guard: `PlanReplace` calls `checkOmissions` (Core-fixes step 7) on the staged files before it returns, as `stage` does. Test `TestPlanReplaceRefusesOmissionLine` in `internal/tool/patch/replace_test.go`: a replacement text `// ... existing code ...` for a Go file gives the Core-fixes step 7 error text and writes nothing. Source: Core-fixes step 7.
- Lands after: Core-fixes step 7. Refuse omission placeholders in edits (product decision 15).

#### Code rewrite step 4. Match and template expansion

Anchor: Edsger W. Dijkstra

- Files: new `internal/tool/rewrite/template.go`, new `internal/tool/rewrite/template_test.go`, new `internal/tool/rewrite/match.go`, new `internal/tool/rewrite/match_test.go`. The functions are pure. They do no file I/O and make no runner calls.
- Symbols and rules:
  - `type part struct { text, capture string }` and `type template []part`.
  - `func parseTemplate(text string) (template, error)`. The grammar: `@@` is one literal `@`. `@` followed by one or more ASCII letters, digits or `_` is a capture part with that name. `@{` followed by one or more ASCII letters, digits, `_` or `.`, then `}`, is a capture part with the name between the braces. Every other `@` returns `rewrite: replace has a lone @ at byte <N>; write @@ for a literal @`, where `<N>` is the byte offset of that `@`. Text other than `@` is literal.
  - `func (t template) captures() []string` returns the capture names in order.
  - `type span struct { start, end int }` and `type group struct { line int; spans map[string]span }`.
  - `func groupMatches(captures []syntax.Capture) ([]group, error)` groups the captures by their `Match` field, which the frozen `syntax.Capture` has (Code symbols Schemas). Groups keep the order of first appearance. `group.line` is the `Line` of the first capture of the match. A name that occurs twice in one match returns `rewrite: @<name> occurs twice in one match at line <N>`, where `<N>` is the `Line` of the second capture.
  - `func expand(t template, src []byte, g group) ([]byte, error)` copies literal text, and it copies `src[start:end]` for a capture part. A name missing from `g.spans` returns `rewrite: @<name> is not in the match at line <N>`, with `<N>` set to `g.line`.
  - `type edit struct { start, end, line int; text []byte }`.
  - `func editsOf(path string, src []byte, groups []group, target string, t template) ([]edit, error)`. For each group, in order: when `g.spans` has no `target`, return `rewrite: target <name> is not in the match at line <N>`. When the target span is empty, return `rewrite: target <name> has zero width at line <N>`. Expand `t` into `text`, and append `edit{start, end, line: g.line, text}`. Sort the edits by `start`, then by `end`. When `prev.end > cur.start` for adjacent edits, return `rewrite: targets overlap in <path> at line <cur.line>`.
  - `func apply(src []byte, edits []edit) []byte` builds the post-image by applying the sorted, non-overlapping edits.
  - `func splicesOf(edits []edit) []snapshot.Splice` returns `snapshot.Splice{Start, End, Inserted: len(text)}` for each edit, in pre-image coordinates (`internal/tool/snapshot/store.go:38-42`).
- Tests that fail before the change:
  - `TestParseTemplate`, a table in `template_test.go`. `a@@b` gives the literal `a@b`. `@name` gives the capture `name`. `x@name.y` gives the literal `x`, the capture `name`, then the literal `.y`. `@{definition.function}` gives the capture `definition.function`. `@name.` gives the capture `name`, then the literal `.`. `@ x` returns `rewrite: replace has a lone @ at byte 0; write @@ for a literal @`. `@{}` and `@{name` return the same text with byte 0. Source: the grammar of this step.
  - `TestExpandCaptureAndLiteralAt`. Source `func Hi() {}`. Group with `name` span `[5, 7)`. Template `@name@@`. Expected `Hi@`. Source: the expansion rule.
  - `TestEditsOfReplacesTargetOnly`. Source `func A() {}\nfunc B() {}\n`. Two groups with `name` spans `[5, 6)` at line 1 and `[17, 18)` at line 2. Target `name`, template `X`. Expected `func X() {}\nfunc X() {}\n`. Source: the replacement rule.
  - `TestEditsOfRefusesAbsentTarget`. A group with only `other`, target `name`, at line 3. Expected `rewrite: target name is not in the match at line 3`.
  - `TestEditsOfRefusesZeroWidthTarget`. A group with `name` span `[5, 5)` at line 3. Expected `rewrite: target name has zero width at line 3`.
  - `TestEditsOfRefusesOverlap`. Source name `a.go`. Two groups at line 1 with `name` spans `[0, 10)` and `[5, 15)`. Expected `rewrite: targets overlap in a.go at line 1`.
  - `TestEditsOfRefusesAbsentCapture`. Template `@other`, a group with only `name`, at line 3. Expected `rewrite: @other is not in the match at line 3`.
  - `TestGroupMatchesRefusesRepeatedName`. Two captures named `name`, both with `Match` 0 and `Line` 1. Expected `rewrite: @name occurs twice in one match at line 1`.
  - `TestGroupMatchesSplitsByMatch`. Captures `name` with `Match` 0 at line 1, and `name` with `Match` 1 at line 2. Expected two groups, at lines 1 and 2.
  - `TestApplyAndSplicesOf`. Source `abc`. Edits `{0, 1, "X"}` and `{2, 3, "YY"}`. Expected `apply` returns `XbYY`, and `splicesOf` returns `[{0 1 1} {2 3 2}]`. Source: byte arithmetic.
- Specification text: none.
- Lands after: Code rewrite step 1.

#### Code rewrite step 5. The rewrite tool

Anchor: Christopher Alexander, Dieter Rams

- Files: new `internal/tool/rewrite/rewrite.go`, new `internal/tool/rewrite/rewrite_test.go`, `internal/app/app.go` (the tool list at line 150 gets `rewrite.New(),` after `patch.New(cfg.EditStyle.EditStyle(modelID)),`), `internal/app/app_test.go` (one test beside `TestToolSetAppendsBatteryToolsForEveryModel`, line 307), `internal/script/eval_test.go` (one test beside `TestEvalFirstFoldsToolsIntoEval`, line 154), new `docs/spec/rewrite.md`, `docs/spec/secrets.md` (the sentences of `Specification of masking` below), `docs/README.md` (one line after the `Edit styles` line, `- [Edit styles](spec/edit-styles.md): parser, resolver, writer, and refusal contracts.`).
- Symbols:
  - `type Editor struct{}`, `func New() tool.Editor`, and the methods `Spec() tool.Spec`, `Class() tool.Class` (returns `tool.ClassEdit`), `Run` and `Plan`.
  - `type args struct { Lang string json:"lang"; Pattern string json:"pattern"; Target string json:"target"; Replace string json:"replace"; Path string json:"path"; Glob string json:"glob" }`.
  - `func checkStopped(path string, r syntax.Report) error` returns `rewrite: parse stopped for <path>; nothing was written.` when `r.Stopped` is true.
  - Constants: `rewriteMaxFileBytes = 1 << 20`, `rewriteMaxScanned = 256`, `rewriteMaxFiles = 32`, `rewriteMaxEdits = 256`, `rewriteDeadline = 90 * time.Second`.
  - Sentinel: `errRewriteDeadline = errors.New("rewrite: deadline reached")`.
- Change:
  - `Run` calls `Plan`, then `plan.Apply(ctx)`, as `patch.Editor.Run` does (`internal/tool/patch/patch.go:26-31`).
  - `Plan` runs these steps in order. Each refusal returns at once.
    1. Decode the arguments with `json.Unmarshal(call.Args, &a, json.RejectUnknownMembers(true))` from `encoding/json/v2`. An error returns `rewrite: invalid arguments: <err>`. The option exists at `options.go:237` of the Go 1.27.0 toolchain source, `golang.org/toolchain@v0.0.1-go1.27.0.linux-amd64/src/encoding/json/v2/options.go`.
    2. When `env.Masker` is not nil, replace `Pattern` and `Replace` with their `UnmaskStrict` results. An error returns `rewrite: secret placeholder cannot be restored: <err>`. Keep the masked pattern for the no-match text.
    3. `syntax.ByName(a.Lang)` must succeed. Otherwise return `rewrite: unknown lang <name>, use one of: <syntax.Names() joined by ", ">`. An empty `Pattern` returns `rewrite: pattern must not be empty`. An empty `Target` returns `rewrite: target must not be empty`.
    4. `q, err := lang.Compile(pattern)`. An error returns `rewrite: ` followed by the error text of `Compile`, which starts with `syntax: `.
    5. `Target` must be in `q.CaptureNames()`. Otherwise return `rewrite: target <name> is not a capture of the query`.
    6. `parseTemplate(replace)`. Each name from `captures()` must be in `q.CaptureNames()`. Otherwise return `rewrite: replace names @<name>, which the query does not capture`.
    7. `tool.IsLocalRunner(env.Runner)` must be true. Otherwise return `rewrite: the rewrite tool needs the local runner; this runner is not local`.
    8. `ctx, cancel := context.WithTimeoutCause(ctx, rewriteDeadline, errRewriteDeadline)`, with `defer cancel()`. Create one parser with `lang.NewParser()` for the call.
    9. `paths, err := search.Select(ctx, env.Cwd, a.Path, a.Glob, lang.Extensions())`. When `len(paths) > 256`, return `rewrite: <n> files match; the limit is 256. Narrow path or glob.`. When `len(paths)` is 0, return the no-match result of this step.
    10. For each path, in order, where `abs := filepath.Join(env.Cwd, filepath.FromSlash(path))`:
        - `info, err := env.Runner.Stat(ctx, abs)`. When `info.Size > rewriteMaxFileBytes`, return `rewrite: <path> is over 1048576 bytes; narrow path or glob.`.
        - `src, err := env.Runner.ReadFile(ctx, abs)`.
        - `caps, rep, err := parser.Captures(ctx, q, src)`. An error is returned unchanged, so a cancelled call returns the cancel cause. Then `checkStopped(path, rep)`.
        - When `rep.SyntaxErrors` is true, return `rewrite: <path> has syntax errors; fix it with patch first`.
        - `groups, err := groupMatches(caps)`, then `edits, err := editsOf(path, src, groups, a.Target, template)`. When there are no edits, continue.
        - `post := apply(src, edits)`. When `bytes.Equal(post, src)`, continue.
        - `_, repAfter, err := parser.Captures(ctx, q, post)`. Then `checkStopped(path, repAfter)`. When `repAfter.SyntaxErrors` is true, return `rewrite: <path> has a syntax error at line <N> after the rewrite; nothing was written.`, with `<N>` set to `repAfter.ErrorLine`.
        - Record the file with its `src`, `post` and `edits`.
        - When `ctx.Err()` is not nil, return `context.Cause(ctx)`.
    11. When no file changed, return the no-match result. When more than 32 files changed, return `rewrite: <n> files would change; the limit is 32. Narrow path or glob.`. When more than 256 edits would change, return `rewrite: <n> replacements would change; the limit is 256. Narrow path or glob.`.
    12. The summary is `rewrite: <n> replacements in <m> files\n`, then one line `<path>: <k> replacements\n` for each changed file, in path order. Call `patch.PlanReplace(ctx, env, summary, files)` with one `patch.ReplacedFile{Path, Before: src, After: post, Splices: splicesOf(edits)}` for each changed file, and return its plan.
  - The no-match result is a plan with no files, whose `Apply` returns `tool.Result{Output: []byte("No matches for " + pattern), OK: true}`, where `pattern` is the masked text the call wrote. Its `Discard` is `func() {}`.
  - `Spec()` returns `tool.Spec{Name: "rewrite", Description: <text in Schemas>, Schema: <schema in Schemas>, Keep: tool.KeepHead}`.
- Tests that fail before the change, in `internal/tool/rewrite/rewrite_test.go`. Each test builds its tree in `t.TempDir()`, and its env as `editTestEnv` does (`internal/tool/patch/property_test.go:22-30`): `runner.NewLocal(filepath.Join(t.TempDir(), "runs"))`, `Cwd` set to the tree, and `Snapshots: snapshot.New()`.
  - `TestRewriteRenamesCalledFunctionInTwoFiles`. Files `a/a.go`: `package a\n\nfunc Old() int { return 1 }\n\nfunc Use() int { return Old() }\n`, and `b/b.go`: `package b\n\nfunc Caller() int { return Old() }\n`. Arguments: `lang` `go`, pattern `((identifier) @name (#eq? @name "Old"))`, target `name`, replace `New`. Expected: `a/a.go` holds `func New()` and `return New()`, `b/b.go` holds `return New()`, and the result text starts with `rewrite: 3 replacements in 2 files\na/a.go: 2 replacements\nb/b.go: 1 replacements\n`. Source: the input, and step 12 of this step. The `#eq?` predicate is supported (`github.com/odvcencio/gotreesitter@v0.57.0/query.go:117`).
  - `TestRewriteExpandsCaptureAndLiteralAt`. File `a.go`: `package a\n\n// Hi\n`. Pattern `(comment) @name`, target `name`, replace `// @name @@`. Expected file: `package a\n\n// // Hi @\n`. Source: the grammar of step 4 and the Go comment node.
  - `TestRewriteReplacementThatStillMatchesRunsAgain`. File `a.go`: `package a\n\n// Hi\n`. Pattern `(comment) @name`, target `name`, replace `// @name`. First call: the file holds `package a\n\n// // Hi\n`. Second call with the same arguments: the file holds `package a\n\n// // // Hi\n`. Source: decision 27 of this workstream.
  - `TestRewriteSplicesBytesInCRLFFile`. File `a.go`: `package a\r\n\r\nvar s = 1\r\nvar t = 2\r\n`. Pattern `(int_literal) @name`, target `name`, replace `3 /* x\n y */`. Expected file: `package a\r\n\r\nvar s = 3 /* x\n y */\r\nvar t = 3 /* x\n y */\r\n`. Source: decision 20 of this workstream.
  - `TestRewriteRefusesBadQuery`. Pattern `(function_declaration`. Expected: the error text `rewrite: ` followed by the error text that `ByName("go").Compile("(function_declaration")` returns. Source: the library message.
  - `TestRewriteRefusesUnknownLang`. `lang` `cobol`. Expected: `rewrite: unknown lang cobol, use one of: bash, c, c_sharp, cpp, css, go, html, java, javascript, json, kotlin, lua, markdown, php, python, ruby, rust, svelte, swift, toml, tsx, typescript, yaml`. Source: `syntax.Names()` of Code symbols Schemas.
  - `TestRewriteRefusesUnknownTarget`. Target `missing`. Expected: `rewrite: target missing is not a capture of the query`.
  - `TestRewriteRefusesUnknownReplaceCapture`. Replace `@other`. Expected: `rewrite: replace names @other, which the query does not capture`.
  - `TestRewriteRefusesLoneAt`. Replace `a @ b`. Expected: `rewrite: replace has a lone @ at byte 2; write @@ for a literal @`.
  - `TestRewriteRefusesOverlappingTargets`. File `a.go`: `package a\n\nfunc F() {\n\tif true {\n\t}\n}\n`. Pattern `(block) @name`, target `name`, replace `{}`. Expected: `rewrite: targets overlap in a.go at line 4`. Source: the outer block starts on line 3, and the inner block starts on line 4.
  - `TestRewriteRefusesFileWithSyntaxErrorsBefore`. File `a.go`: `package a\n\nfunc Keep( {\n`. Pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `Renamed`. Expected: `rewrite: a.go has syntax errors; fix it with patch first`, and the file keeps its input bytes. Source: the owner's ruling.
  - `TestRewriteSyntaxBreakLeavesFilesUnchanged`. Files `a.go`: `package a\n\nfunc Keep() int { return 1 }\n` and `b.go`: `package b\n\nfunc Other() int { return 2 }\n`. Pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `Renamed(`. Expected: `rewrite: a.go has a syntax error at line 3 after the rewrite; nothing was written.`, and both files hold their input bytes. Source: the line of the inserted text in `a.go`.
  - `TestRewriteStoppedParseIsRefused`. Calls `checkStopped("a.go", syntax.Report{Stopped: true})`. Expected: `rewrite: parse stopped for a.go; nothing was written.`. Source: the stopped-parse rule.
  - `TestRewriteRefusesOversizeFile`. File `a.go`: `package a\n\nvar s = "` followed by 1048576 letters `a`, then `"\n`. Pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `X`. Expected: `rewrite: a.go is over 1048576 bytes; narrow path or glob.`. Source: `rewriteMaxFileBytes`.
  - `TestRewriteRefusesTooManyFiles`. Files `pkg/fNN.go` for NN from 00 to 32, each `package p\n\nfunc Target() {}\n`. Pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `Other`, path `pkg`. Expected: `rewrite: 33 files would change; the limit is 32. Narrow path or glob.`. Source: `rewriteMaxFiles`.
  - `TestRewriteRefusesTooManyEdits`. File `a.go` with 257 lines `func F<i>() {}` for i from 0 to 256, after `package a\n\n`. Pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `G`. Expected: `rewrite: 257 replacements would change; the limit is 256. Narrow path or glob.`. Source: `rewriteMaxEdits`.
  - `TestRewriteRefusesTooManyCandidates`. Files `pkg/fNNN.go` for NNN from 000 to 256, each `package p\n`. Pattern `(package_clause (package_identifier) @name)`, target `name`, replace `q`, path `pkg`. Expected: `rewrite: 257 files match; the limit is 256. Narrow path or glob.`. Source: `rewriteMaxScanned`.
  - `TestRewriteNoMatch`. File `a.go` with `package a\n\nfunc Hi() {}\n`. Pattern `(function_declaration name: (identifier) @name (#eq? @name "Missing"))`, target `name`, replace `X`. Expected result text: `No matches for (function_declaration name: (identifier) @name (#eq? @name "Missing"))`, and the file is unchanged. Source: the no-match rule.
  - `TestRewriteRefusesNonLocalRunner`. `Env.Runner` is `struct{ runner.Runner }`. Arguments are valid: `lang` `go`, pattern `(function_declaration name: (identifier) @name)`, target `name`, replace `X`. Expected: `rewrite: the rewrite tool needs the local runner; this runner is not local`.
  - `TestRewriteSchemaLangMatchesGrammars`. Expected: the `enum` of the `lang` property in the schema equals `syntax.Names()`, element by element. Source: the schema rule of this workstream.
  - `TestRewritePlanIsDeterministic`. Two `Plan` calls with the same arguments and tree. Expected: equal `Files` (paths, `PreSHA`, `PostSHA`) and equal `Pre` bytes. Source: `search.Select` returns sorted paths.
  - `TestRewriteStaleFileRefusedAtApply`. Plan on `a.go`, then the file is written with other bytes, then `Apply`. Expected: `patch: a.go changed while you were approving. Nothing was written. Try again`, and the file keeps the outside bytes. Source: `verifySource` in `internal/tool/patch/writer.go:81`.
  - `TestToolSetListsRewriteAfterPatch` in `internal/app/app_test.go`. Calls `toolSet(config.Config{EditStyle: config.EditStyleSpec{Default: "anchor"}}, func(context.Context, string) []tool.Tool { return nil })`, invokes the returned `agent.ToolSet` with a context, the model ID `gpt-test` and the working directory `/project`, and reads each `Spec().Name`. Expected: `rewrite` at the index that follows `patch`. Source: the tool list at `internal/app/app.go:150`.
  - `TestRewriteIsReachableFromCells` in `internal/script/eval_test.go`. Mode `config.ToolModeEvalFirst`. `Tools` is an echo tool named `rewrite` (`echoTool`, `eval_test.go:33`). Cell: `tools.rewrite(lang = 'go', pattern = 'x', target = 'x', replace = 'y')['tool']`. Expected: `"rewrite"`. Source: `echoTool`, and the cell rule of Eval default step 1 in `drafts/eval-default-v2.md`.
- Specification text, `docs/spec/rewrite.md`. Each sentence has at most 25 words, and it uses MUST in the RFC 2119 sense:
  - The `rewrite` tool MUST replace the text of one named capture in each match of a tree-sitter query.
  - The tool is an edit-class tool, and it MUST apply its edits through the approval, stale-file guard, journal and rollback that `patch` uses.
  - The tool MUST parse each candidate file before it plans an edit.
  - The tool MUST refuse a call when a candidate file has syntax errors before the call.
  - The tool MUST refuse a call when a changed file has syntax errors after the rewrite.
  - A refused call MUST NOT write any file.
  - The tool MUST refuse a candidate file over 1048576 bytes and MUST name that file.
  - The tool MUST refuse a call that selects more than 256 files, changes more than 32 files, or makes more than 256 replacements.
  - The tool MUST refuse a call on a runner that is not local.
  - The tool MUST refuse a match whose target has zero width.
  - The tool MUST refuse two targets that overlap, and it MUST name the file and the line.
  - The tool MUST refuse `@name` when the match has no capture of that name.
  - The tool MUST write `@@` as one literal `@`.
  - The tool MUST refuse any `@` that does not start `@@`, `@name` or `@{name}`.
  - The tool MUST splice replacement bytes as written, and it MUST NOT convert line endings.
  - A replacement that still matches the query MUST be allowed, and each call changes the file again.
  - The tool MUST unmask secret placeholders in `pattern` and `replace` only.
  - The tool MUST NOT unmask `path` or `glob`.
  - The tool MUST stop planning when 90 seconds have passed.
  - The result MUST start with a summary line, then one line for each changed file in path order.
  - The result MUST then give the post-image of each changed file.
  - The tool MUST NOT offer a preview flag, because the `syntax` mode of `search` is the preview.
  - The `syntax` preview MUST list a file over its own size cap in its footer.
  - A call with no changed file MUST return the text `No matches for` followed by the pattern.
- Specification of masking: `docs/spec/secrets.md` is among the files of this step. In the two sentences that Specifications step 32 writes for the tools that unmask, the subject `The exec and patch tools` becomes `The exec, patch and rewrite tools`, so they read: `The exec, patch and rewrite tools MUST unmask with UnmaskStrict.` and `The exec, patch and rewrite tools MUST fail the call when a placeholder is unknown.` One sentence is added after them: `The rewrite tool MUST unmask its pattern and replace arguments only, and MUST NOT unmask a path or a glob.`
- Lands after: Code rewrite step 2, Code rewrite step 3, Code rewrite step 4, Eval default step 1, Specifications step 32. Correct `docs/spec/secrets.md` and Specifications step 40.

#### Code rewrite step 6. Rewrite approval, wire and journal tests

Anchor: Edward Tufte, John Carmack

- Files: new `internal/rpc/serve_rewrite_test.go`, new `internal/agent/rewrite_test.go`, `internal/agent/harness_test.go` (the method `tools` at line 272 appends the tools of a new field `extraTools []tool.Tool` after its four fixed tools; a zero value keeps every existing test as it is).
- Change: no production code. A rewrite call already gets the file-change kind from its class (`internal/rpc/serve_items.go:147-148`). Its diff comes from the blob-based `fileChanges` of Protocol step 4, and its approval diff comes from Protocol step 5, which reads the planned files for every `tool.ClassEdit` call.
- Tests. Each test fails before step 5, because the tool is absent from the tool list:
  - `TestRewriteAskApprovalSendsDiff`, in `internal/rpc/serve_rewrite_test.go`. The rig of the test that Protocol step 5 adds for `TestEditApprovalSendsTheDiffFirst`, with `config.ApprovalAsk`, the tools `read.New()`, `search.New()` and `rewrite.New()`, and the file `a.go` holding `package a\n\nfunc Old() {}\n`. Call: `{"lang":"go","pattern":"((identifier) @name (#eq? @name \"Old\"))","target":"name","replace":"New"}`. Expected: `item/fileChange/patchUpdated` comes before `item/fileChange/requestApproval`, and `changes[0]` has `additions` 1 and `deletions` 1. Source: Protocol step 5 and the one changed line.
  - `TestRewriteCompletesAsFileChange`, in `internal/rpc/serve_rewrite_test.go`. The same call and file with `config.ApprovalAuto`. Expected: the completed item has the type `fileChange`, `kind.type` `update`, `changes[0]` with `additions` 1 and `deletions` 1, and the `thread/resume` snapshot holds the identical item. Source: the shape that Protocol step 4 defines.
  - `TestRewriteStartedHoldsPreImage`, in `internal/agent/rewrite_test.go`. The harness from `newHarness` (`internal/agent/harness_test.go:219`), with `withApproval(config.ApprovalAuto)` (`approval_test.go:68`) and `extraTools` set to `rewrite.New()`. File `a.go` holds `package a\n\nfunc Old() {}\n`. The rewrite renames `Old` to `New`. Expected: the journaled `tool.started` event has one file, and its `PreBlob` holds the bytes `package a\n\nfunc Old() {}\n`, read through the journal's `Blob` method (`internal/agent/recover.go:325`). Source: `started` at `internal/agent/tools.go:319` and the pre-image at `tools.go:343`.
- Specification text: none in this step. The `rewrite` item uses the file-change paragraph that Protocol step 5 writes.
- Lands after: Code rewrite step 5, Protocol step 4 and Protocol step 5.

#### Schemas of the Code rewrite workstream

The `rewrite` tool schema. The `lang` enum holds the 23 names of `syntax.Names()`, sorted. `additionalProperties` is false. The schema is built once at package init with `encoding/json/v2`.

```
{"type":"object","properties":{"lang":{"type":"string","enum":["bash","c","c_sharp","cpp","css","go","html","java","javascript","json","kotlin","lua","markdown","php","python","ruby","rust","svelte","swift","toml","tsx","typescript","yaml"]},"pattern":{"type":"string"},"target":{"type":"string"},"replace":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"}},"required":["lang","pattern","target","replace"],"additionalProperties":false}
```

The description, exact text:

```
Rewrite code with a tree-sitter query. lang names the grammar. pattern is a tree-sitter query, such as (function_declaration name: (identifier) @name). target names the capture whose text is replaced, without @. replace is the new text. @name inserts the text of that capture in the same match. @{name} does the same for a name with dots, such as @{definition.function}. @@ writes a literal @. path and glob select files as search does. Replacement bytes are spliced as written, so the tool does not convert line endings. The call writes nothing and refuses when the query does not compile, when a file has syntax errors before or after the call, when two targets overlap, or when a limit is exceeded. A replacement that still matches the query is allowed, and a second call changes the file again. Preview the matches with search, mode syntax, and the same lang, pattern, path and glob. Edits go through the same approval, journal and review as patch.
```

The package API:

```
package rewrite
func New() tool.Editor
type Editor struct{}
func (Editor) Spec() tool.Spec
func (Editor) Class() tool.Class
func (Editor) Plan(ctx context.Context, call tool.Call, env tool.Env) (tool.Plan, error)
func (Editor) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error)
```

The additions to other packages:

- `internal/syntax`: `Report` gains `ErrorLine int`. Also `func (q *Query) CaptureNames() []string` and `func (l Language) Extensions() []string`. Code rewrite step 1 adds them.
- `internal/tool/search`: `func Select(ctx context.Context, cwd, path, glob string, exts []string) ([]string, error)`. Code rewrite step 2 adds it.
- `internal/tool/patch`: `type ReplacedFile struct { Path string; Before []byte; After []byte; Splices []snapshot.Splice }` and `func PlanReplace(ctx context.Context, env tool.Env, summary string, files []ReplacedFile) (tool.Plan, error)`. Code rewrite step 3 adds them.

The error texts, exactly as the code returns them:

- `rewrite: invalid arguments: <err>`
- `rewrite: secret placeholder cannot be restored: <err>`
- `rewrite: pattern must not be empty`
- `rewrite: target must not be empty`
- `rewrite: unknown lang <name>, use one of: <names>`
- `rewrite: ` followed by the error text of `Compile`, which starts with `syntax: `
- `rewrite: target <name> is not a capture of the query`
- `rewrite: replace has a lone @ at byte <N>; write @@ for a literal @`
- `rewrite: replace names @<name>, which the query does not capture`
- `rewrite: the rewrite tool needs the local runner; this runner is not local`
- `rewrite: <n> files match; the limit is 256. Narrow path or glob.`
- `No matches for <pattern>` (a success, with no file in the plan)
- `rewrite: <path> is over 1048576 bytes; narrow path or glob.`
- `rewrite: parse stopped for <path>; nothing was written.`
- `rewrite: <path> has syntax errors; fix it with patch first`
- `rewrite: <path> has a syntax error at line <N> after the rewrite; nothing was written.`
- `rewrite: target <name> is not in the match at line <N>`
- `rewrite: target <name> has zero width at line <N>`
- `rewrite: targets overlap in <path> at line <N>`
- `rewrite: @<name> is not in the match at line <N>`
- `rewrite: @<name> occurs twice in one match at line <N>`
- `rewrite: <n> files would change; the limit is 32. Narrow path or glob.`
- `rewrite: <n> replacements would change; the limit is 256. Narrow path or glob.`
- `rewrite: deadline reached`
- `patch: <path> changed while the rewrite was planned. Nothing was written. Try again`
- `patch: <path> changed while you were approving. Nothing was written. Try again` (apply, existing text)

The result text, for two files. Each post-image is a tagged section, as `patch` prints it:

```
rewrite: 3 replacements in 2 files
a/a.go: 2 replacements
b/b.go: 1 replacements
[a/a.go#<TAG>]
1:package a
...
[b/b.go#<TAG>]
1:package b
...
```

The constants:

```
rewriteMaxFileBytes = 1 << 20
rewriteMaxScanned = 256
rewriteMaxFiles = 32
rewriteMaxEdits = 256
rewriteDeadline = 90 * time.Second
```

The repository rule that `just lint` checks: `.golangci.yml:105` puts `**/internal/tool/**` in the `core` files list, so the new packages get the core depguard rules.

#### Checks of the Code rewrite workstream

Each step runs `just verify` before its commit. From Specifications step 40 on, `just verify` runs `spec-style` (`justfile:121` changes to `verify: fmt-check vet lint test vuln hashline-model spec-style web-check cf-check`), so step 5 needs no separate style command. `just lint` covers the new packages through the `core` files list at `.golangci.yml:105`. Lint runs `unused` (`.golangci.yml:21`), and the file sets no `tests` key, so a function that only a test calls counts as used. For the steps that add code under `internal/syntax`, `internal/tool/search`, `internal/tool/patch` or `internal/tool/rewrite`, the main agent also runs `go test -count=1 ./internal/syntax/... ./internal/tool/search/... ./internal/tool/patch/... ./internal/tool/rewrite/...` and `go test -race -count=1 ./internal/tool/rewrite/ ./internal/tool/patch/`. Step 6 also runs `go test -count=1 ./internal/rpc/... ./internal/agent/...`. The main agent runs these checks. This workstream does not run them.

#### Prerequisites of the Code rewrite workstream

Code symbols step 8 lands first. Steps 1 and 2 need the package `internal/syntax` and the modes of `search` that it defines (`Compile`, `Captures`, the `Report` type and the extension table), and step 2 changes the candidate listing that Code symbols step 6 creates in `internal/tool/search/parse.go`. Eval default step 1 (`drafts/eval-default-v2.md`) lands before step 5, because `TestRewriteIsReachableFromCells` needs cells to reach every tool except `eval` and `yield`. Specifications step 40 (the `spec-style` gate) lands before step 5, so that `just verify` checks `docs/spec/rewrite.md`. Protocol step 4 (blob-based `fileChanges` and `FileChange.Post`) and Protocol step 5 (the approval diff for `tool.ClassEdit`) land before step 6. No new module dependency is added. `encoding/json/v2` is already used in `internal/agent/gate.go`, and `internal/syntax` is the only importer of the library.

