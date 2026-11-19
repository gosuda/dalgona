### Work added by the reviews

Four reviews and one adversarial review read this plan. Their exact text changes are already in the steps. The work below is extra: each item belongs to the step it names and lands in that step's commit, with its test failing first.

#### From the owner's view

| Step | Work |
|---|---|
| Pins step 5 | Run the OSV batch query for every row of the pins table, including go-udiff v0.4.1, and record each result in the table that Pins step 5 writes. |
| Web step 3 | Add a web test that validates each `dalgona/` fixture against `internal/rpc/testdata/dalgona-schema` with ajv, and see it fail before the validation exists. |
| Ask step 3 | Add `TestQuestionDeclinesWhenEveryWindowCannotAnswer` in `internal/agent`: two subscribers both answer with an error, and the result is the no-client text. |
| Terminal step 11 | Add a terminal test that the tick chain runs in the state `awaiting input` and follows the motion setting. |
| Specifications step 40 | Read every changed file under `docs/spec` once for active voice, present tense and the terms list before the style check becomes a gate. |
| Terminal step 10 | Check with `grep -n keywordSpans internal/tui` that no local keyword matcher remains after Terminal step 10 lands. |

#### From the design view

| Step | Work |
|---|---|
| Terminal step 2 | In the Step 2 test TestUntrustedTextIsSanitizedAtEveryEntry, add a row for command/exec stdout and stderr and a row for a fuzzyFileSearch file name that holds an escape byte, expecting the visible text in View().Content and no OSC sequence. |
| Terminal step 11 | Add TestNoticeShowsDuringTurn to dock_test.go (Terminal step 11): with state running, the refusal text of Terminal step 6 appears in the live row, and the next key press removes it. |
| Terminal step 11 | Add TestLiveRowBelowFortyColumns to dock_test.go (Terminal step 11): at 30 columns every live-row line has cells at most 29. |
| Terminal step 12 | Add TestStatusBandFolderCutsAtSixteen to band_test.go (Terminal step 12): a 40-cell folder name renders in 16 cells and ends with three dots. |
| Terminal step 12 | Add U+203A with expected width class N to TestBandGlyphsHaveNarrowWidth (Terminal step 12), so the prompt mark gets the same width check as the band glyphs. |
| Web steps 29 and 30 | In SettingsPanel.dom.test.ts (Web step 30), expect the keyPaths status_hide and edit_review.enabled, and in ModelPanel.dom.test.ts (Web step 29), expect model_reasoning_effort. |
| Web step 10 | In labels.test.ts (Web step 10), assert that idle gives Ready, that idle after a completed turn gives Done, and that disconnected gives Offline. |
| Web steps 20 and 36 | In App.dom.test.ts (Web step 20), expect the document title dalgona · title · state in every state; and in session.test.ts (Web step 36), assert that an interrupted turn calls notifyAttention. |
| Web step 16 | Add a browser check web/checks/keyword-mark.js that reads the computed background of mark.keyword and expects it transparent in each of the four concrete themes. |
| Terminal step 4 | Add TestConfirmPanelBodyKeepsFourRows (Terminal step 4) at 24 rows: the revert panel shows its title, four body lines and one action line. |
| Terminal step 22 | Add TestPopupStaysOpenOnFirstToken to complete_test.go (Terminal step 22): a slash typed at the start of a line with text after it keeps the popup open and leaves the cursor in the first token. |
| Terminal step 31 | Add TestImagesOverTenMiBRefused (Terminal step 31): two 6 MiB images in one prompt are refused by the 10 MiB request total, with the notice text of the image-input workstream. |

#### From the view of the engineer who executes

| Step | Work |
|---|---|
| Live-debug step 4 | After live-debug step 4 writes docs/spec/live-debug.md, run `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/live-debug.md`, fix every finding without changing a literal, and rerun until it prints `style findings: 0`. |
| Specifications steps 8, 14 and 16 | Before each of these steps applies a row that the specification review did not check against the code, read the code line that the row cites and stop when the code differs. The rows are every row of step 8, rows 63, 153, 345, 370, 426, 457, 496 and 527 to 545 of step 14, and rows 71, 76, 82, 88, 101 and 117 of step 16. |
| Specifications step 24 | In `docs/spec/protocol.md` line 22, replace `The merge MUST keep order.` with `The merge MUST keep the order of the text of that item. A merged delta MAY reach the client before a message that was queued between the two deltas.` The code is `internal/rpc/conn.go:566-576`. |

#### From the engineering and failure views

| Step | Work |
|---|---|
| Edit-review step 4 | Move the diff, masking and state encoding of judgeInput into the goroutine that startReview starts, set w.eligible after group.Wait in joinReviews, and add a test in internal/agent/review_test.go that blocks the masker and asserts tool.completed is journaled before the masker returns. |
| Edit-review step 4 | Start the 8 s timeout context before the factory call in askJudge (internal/agent/review.go) and add TestEditReviewFactoryHangFailsOpen, where a factory that blocks until its context ends gives an error verdict with the text review timed out after 50ms within 2 s. |
| Edit-review step 4a | Build turn skipped paths from the capped paths list in internal/agent/review.go and add TestTurnJudgedSkippedWithinPathCap in internal/agent/event/turn_judged_test.go with 70 changed paths, where the 65th has no post blob, expecting Validate to pass and skipped to omit that path. |
| Edit-review step 4 | Add TestJudgedPathsWithheldWhenMaskingFails in internal/agent/review_test.go, where a failing masker makes tool.judged.paths equal the single entry [paths withheld: masking failed] and no journal byte holds the raw path. |
| Edit-review step 4a | Add TestTurnReviewMasksPaths in internal/agent/review_turn_test.go, where a secret in a changed path appears in turn.judged.paths only as its placeholder and not in the journal bytes. |
| Core-fixes step 5 | Add TestAutoCompactionDoesNotRepeatSummarizer in internal/agent/compaction_test.go, where two steps of one turn with a non-shrinking summary call the summarizer once. |
| Protocol step 13 | Give asktool.Question a field that suppresses the Other row, set it for the sandbox question in internal/agent/sandbox_choice.go, and add TestSandboxQuestionHasNoOtherRow in internal/rpc/serve_input_test.go that asserts isOther is false on the wire. |
| Protocol step 13 | Make concurrent sandbox fallbacks against one parent share one pending Ask in internal/agent/sandbox_choice.go, and add TestSiblingsShareOneSandboxQuestion where two child exec calls see one question id. |
| Image-input step 2 | Add TestImagePathRejectsFIFO in internal/rpc/serve_image_test.go (skipped on Windows) that sends turn/start with a FIFO path and expects -32602 with image path must be a regular file within one second. |
| Ask step 2 | Add TestAskQuestionTextIsMasked in internal/agent/ask_tool_test.go, where a secret in a question header or option label reaches the subscriber only as its placeholder. |
| Image-input step 5 | Cache each verified image blob on the transcript in internal/agent/image.go and add TestImageBlobReadOncePerThread that counts journal reads across two model steps. |
| Ratchet step 7 | Make recovery of an open ratchet approve complete as interrupted through the first-statement check in recoverStarted (internal/agent/recover.go), and add TestRecoverInterruptsOpenRatchetApprove that expects ok=false and no new question. |
| Protocol step 12 | Verify whether ProviderModels reads the stored credential in internal/agent/access.go; if it does, pass the candidate key and call SetAPIKey only after success, and add a test in internal/rpc that leaves the store byte-identical after a failed verification. |
| Protocol step 19 | Keep each root's file list for 5 seconds in internal/rpc/serve_fuzzy.go and add TestFuzzyFileSearchReusesWalk that counts walks over two requests in the same window. |
| Protocol step 18 | Add a total stat budget across watches to the polling loop in internal/rpc/serve_fs.go and add TestFsWatchBudgetAcrossWatches that checks the per-poll entry count. |
| Terminal step 3 | Add the print tests of the terminal print rules: the view stays frozen while a print is in flight, a print waits for a settled frame, and the dock leaves one row of room. |
| Terminal step 3 | State the five print rules in `docs/spec/tui.md`, next to the width rule. |

#### Later reviews of the changes of 2026-11-12

Four reviews read the six changes that the owner added on 2026-11-12: the print path, the edit review split, the code search index, the code symbols, the eval default and the comment removal. The rulings that settled their disputes are in the steps. The tables record each decision, and the work lists name what each accepted decision adds.

#### Decisions of the later reviews

One row per decision. Two reviews that made the same decision share a row. The decision cell starts with Accepted or Overruled. An overruled row names the owner decision or ruling it conflicts with, or the review choice that replaced it.

| Area | Point of view | Decision | Reason | Principle |
|---|---|---|---|---|
| Terminal print path | CEO, Design | Accepted: keep the inline dock and tea.Println. The normal run uses no alternate screen. | The alternate screen ends native scrollback. The dock keeps selection and scrollback. | impact |
| Terminal print path | CEO | Accepted: print rules 2 and 3 are a workaround. Delete them when a Bubble Tea release contains the fix in PR 1741. | Issue 1881 is open. PR 1741 printed 3 of 3 times in tmux, and v2.1.0 printed 0 of 3. | reversibility |
| Terminal print path | CEO, Ruling | Accepted: keep the frozen view and the 50 ms settle delay. | The ruling measured dock lines left in history in 60 of 60 runs without the frozen view and in 0 of 60 runs with it. | impact |
| Terminal print path | Design | Accepted: the settle wait counts dock height, window size and AltScreen only. Text changes never reset it. | A wait that counted text would hold final blocks back until streaming stopped. | impact |
| Terminal print path | CEO | Accepted: the x/vt emulator test is a regression gate. Real-terminal checks are the user gate, and the emulator alone does not show the fault fixed. | The fault is in screen placement, which a byte stream test cannot show. | risk |
| Terminal print path | CEO | Accepted: check that PrintlnRaw is absent from bubbletea v2.1.1 before the print step starts. If it exists, re-evaluate print rules 2 and 3. | Issue 1666 asks for a raw print, and the plan does not say whether it landed. | sequencing |
| Terminal print path | CEO | Accepted: keep the one-cell margin on printed lines, which is width minus one. | Issue 1885 erases the last character of full-width lines and miscounts double-width rows. | clarity |
| Terminal print path | DX | Accepted: commit and drain are the only path to scrollback. No caller calls tea.Println. | One function owns keys, width, order and the single print in flight. | clarity |
| Terminal print path | DX, Ruling | Accepted: the window height starts at 0, not 24. No row prints before the first size message. | Print rule 4 gives 0, and Terminal step 3 gives 0 too. A default of 24 lets drain print before any size is known. | risk |
| Terminal print path | DX, Ruling | Accepted: the emulator test keeps its sleeps. Inside a synctest bubble they advance the fake clock, reads go under a mutex, and synctest.Wait may run before a read. | The repository bans sleeps that wait on real goroutines. The bubble makes these sleeps deterministic. The ruling settled this. | risk |
| Terminal print path | Engineering | Overruled by the ruling: replace the emulator test's sleeps with synctest.Wait. | The ruling keeps the sleeps inside the bubble. Wait returns before the fake clock advances, so the 50 ms settle timer would never fire. | risk |
| Terminal print path | Engineering | Overruled by the measured test: end the emulator's copy goroutine before the bubble returns. | The proven test starts no goroutine that reads the emulator, because the run did not block without one. | risk |
| Terminal print path | DX | Accepted: build the emulator and the program inside the bubble, and call tea.WithoutSignals() in every program test. | A goroutine that an outside event can wake is not durably blocked, so Wait can hang on it. | clarity |
| Terminal print path | DX | Accepted: neither the spec nor a comment cites the Bubble Tea godoc or its code path as the alt-screen print rule. The held-row rule holds under either reading. | The godoc says no output prints under the alternate screen, and the handler has no such check. | clarity |
| Terminal print path | DX | Accepted, risk recorded: a non-terminal start has no check, and a missing size message leaves a blank screen. The default fix is an open question. | The DX review's search found no IsTerminal, isatty or ModeCharDevice check in cmd, internal/app or internal/tui. | risk |
| Terminal print path | Design, Ruling | Accepted: a failed start prints the welcome row, with the error on one line. | The welcome text commits only on success, so an error in the live row is never on screen. A blank screen is the worst state. | risk |
| Terminal print path | Design | Accepted: the review header shows the count of finished rows waiting, as a number after the title. | A person in the review sees no sign that the turn still runs. A count says held, not lost. | clarity |
| Terminal print path | Design | Accepted: an empty view below two window rows, with no hint. | A dock cannot show a line in fewer than two rows, so silence is the honest state. | risk |
| Terminal print path | Engineering | Accepted: keep the five print rules as written. Holding rows while AltScreen is open is required. | Bubble Tea queues the print message and offers no print acknowledgement. | impact |
| Terminal print path | Engineering | Accepted: printedMsg means the renderer's insert call returned. A failed write is invisible to the client, and docs/spec/tui.md says so. | The insert error is discarded in Bubble Tea's handler, so no row count proves delivery. | risk |
| Terminal print path | Engineering | Accepted: rows held while a review is open have no count bound. The design record states the cost. | Memory grows with the turn's output while the review is open. Each single write stays within the window. | risk |
| Terminal print path | Engineering | Accepted invariants: Bubble Tea is the only writer while Run is active. Each finished row prints once. No print runs in an update that changes dock height, window size or AltScreen. The dock stays within window height minus 1. View is empty before the first print. | These follow from print rules 1 to 5, and the emulator test can check each one. | clarity |
| Edit review | CEO, Ruling | Accepted: a failed part adds no scores and keeps the flags of the parts that answered. A clean verdict with a failed part becomes uncertain. The unit is an error only when every part fails. | Up to 8 parts run under one 8 s limit. Failing the whole unit would drop every other part's flags. | impact |
| Edit review | Design, DX | Accepted: name the failed part in the error text as `part i of n: <cause>`. The test that expects this text is renamed. | The text shows which part stalled. The old test name says the unit fails, which no longer holds. | clarity |
| Edit review | DX, Design, Engineering | Overruled by the ruling: one failed part fails the whole unit and records no scores, and the worst-part rule applies. | The ruling treats a failed part like a part that was not sent, so the verdict comes from the parts that answered. | risk |
| Edit review | Engineering | Accepted: measure error rates per part and per unit. Replay packParts and combineParts in the held-out measurement. | Each unit sends up to 8 requests, so failures rise with the part count. A measurement that skips packing measures other speech than users see. | risk |
| Edit review | CEO, Ruling | Accepted: speakers stay silent until the step 9 measurement names them. The built-in set is a candidate list only. | The design record says an earlier judge flagged about 8 percent of sound code, and none of its 30 strongest flags held. | risk |
| Edit review | DX | Accepted: the speaker flag lives in the question table. ratchet.json holds measured precision only. A test fails when a speaker has no ratchet row. | Two lists that must agree, with nothing that checks they do. | clarity |
| Edit review | CEO | Accepted: measure false speech per part, not per edit. | Splitting gives each part a chance to speak alone, so a per-edit count hides the extra false flags. | risk |
| Edit review | CEO | Accepted: record per-part latency. If a part's p95 exceeds 4 s, set reviewMaxParts to 4 before the feature is enabled by default. | Four parts run at a time, so eight parts need two rounds inside 8 s. The plan has no judge latency data. | risk |
| Edit review | CEO | Accepted: no per-turn request budget. The feedback cap stays the only cap. | Cost is not the limit. The CEO review estimates about 0.0004 USD per part, and the price may differ by model. A new knob adds surface without a measured need. | impact |
| Edit review | Engineering | Accepted: a host-wide limit on judge requests in flight across threads. It bounds concurrency and is not a request budget, so the no-budget decision stands. | Eight threads, each with eight children and four parts, could send 256 requests at once. | risk |
| Edit review | Engineering | Accepted: a unit loads at most 4 MiB before packing. When the cap stops the load, the unit is marked truncated. | Each side can reach the 1 MiB diff limit, so a large patch could hold about 200 MiB at once. | risk |
| Edit review | Engineering | Accepted: the 8 s wait bound applies per round, and the Outcome says so. | joinReviews waits once per round, so a hanging judge delays each round by up to 8 s. | clarity |
| Edit review | Design | Accepted: a partial review never reads as clean. A clean verdict with unsent hunks reads `No problems in the reviewed part of the diff` in the browser and the summary. | ReviewSummary takes no truncated input today, so a row can read as a full review. | clarity |
| Edit review | DX, Engineering | Accepted: a truncated edit unit is never clean. Its verdict is at most uncertain. | Hunks cut past the eight-part cap were never read. A clean verdict would claim evidence that does not exist. | risk |
| Edit review | DX, Engineering | Accepted: a truncated turn unit never speaks. Its verdict is still journaled. | The incomplete question needs the whole turn, and a cut diff cannot show completeness. | risk |
| Edit review | Design | Accepted: cut the review error text at a character boundary, measured in cells. | A byte cut can split a multi-byte character and leave invalid text in a row. | risk |
| Edit review | Design | Accepted: a flag that no part speaks for is labelled `(not sent to the model)` in its row. | A row that reads Flagged next to no model block makes a person assume the model acted. | clarity |
| Edit review | Design | Accepted: a speaking flag from a partial review carries one feedback line: this review covered part of the edit. | The model acts on a flag without knowing that other hunks went unchecked. | audience |
| Edit review | Design | Accepted: a partial clean verdict prints in the terminal at verbosity high only, beside the uncertain rows. | Coverage gaps show without a row for every normal clean edit. | audience |
| Edit review | DX | Accepted: the packer test takes the question-set size from reviewQuestionSet through decide.RequestSize. | A fixed 6,100 bytes hides a change in the question count. | audience |
| Edit review | DX | Accepted: validate `[edit_review]` feedback ids at host start, not at config load. The error names the key and lists the valid ids. | The question table lives in internal/agent, and config cannot import it. | clarity |
| Edit review | DX | Accepted: the review stays disabled by default, with the one-key off switch. Journal events remain readable after a switch-off. | One key disables the feature without losing history. | reversibility |
| Edit review | CEO | Accepted: ask only sound and the six candidate speakers in the first release. The other questions join when the measurement gives precision rows. | The plan asks 15 questions, and none has precision data yet. | impact |
| Edit review | Engineering | Accepted: speech is judged per part, and the spoken flags are the union over parts. The combined sound score is for display only. | The two-key rule holds on each part's own evidence. | risk |
| Edit review | Engineering | Accepted: keep the post-image race check and the recovery rules. | A changed file yields an error verdict, not a wrong clean one. A crash leaves the call unjudged and fails open. | risk |
| Edit review | Engineering | Accepted invariants: each request encodes to at most 30,000 bytes. A unit sends at most 8 parts, 4 in flight. The judge never approves or rejects an edit. | Each invariant can be tested with a fake judge. | clarity |
| Edit review | Engineering | Accepted: the edit-review decision record names the decider provider as the recipient of edit content. | Edit review is the first path that sends file content to the decider. Masking is best effort. | risk |
| Code search index | Engineering | Accepted invariants: one current index per workspace root, written only by a build child and published only by the parent. A file is skipped only when the index holds it as text, the engine would search it, size, mtime and ctime match, and both times are older than the racy reference. | Each clause maps to a check that a differential test can assert. | clarity |
| Code search index | CEO, Owner, Ruling | Overruled: do not build the trigram index in this plan. The owner fixed codesearch, and the ruling says the baseline benchmark lands first and the index lands whatever the numbers are. | The owner's fixed decision and the ruling settle this. | impact |
| Code search index | CEO | Accepted: pin the tagged codesearch v1.2.0, BSD-3-Clause, and not the v1.3.0-rc.1 pre-release. The cache is bounded per root by the build cap, which is the plan default. | A pre-release is not a pin, and the cache bound answers the review's concern about unbounded growth. | risk |
| Code search index | Design | Accepted: output stays byte-identical with and without the index. One MUST sentence in docs/spec/search.md states it, and a byte-compare test enforces it. | The model learns one behaviour, and a faster call must not change the text. | clarity |
| Code search index | Design | Accepted: the index build starts after the call returns. No call waits for the child that reads every file. | A call that crosses 32 MiB would otherwise stall with no progress shown. | impact |
| Code search index | Design, Engineering | Accepted: the build child writes each index into a new generation directory and syncs it. The parent publishes the generation by renaming CURRENT. A reader whose trigram size does not match drops the index. | A torn index can hide a match, and the reply would then read No matches with no truncation line. | risk |
| Code search index | Engineering, Ruling | Overruled: on Windows, a rename over a mapped file can fail, so the reader keeps the old index and retries. | The ruling sets no index on Windows, so this path never runs there. | risk |
| Code search index | Design, Engineering | Accepted: a library crash stays out of model output. The host log records it, and the retry without the index returns today's output. | An operator needs the crash in a log. The model needs no message. | audience |
| Code search index | Engineering | Accepted: a child that exits on a bad index quarantines it, and the next build waits off a backoff. | A bad trailer makes the reader exit, so each later call pays a crash and a full scan. | risk |
| Code search index | Engineering | Accepted: cap the bytes and memory of one build, and stop before codesearch's 4 GB limit. | The build allocates large sets and buffers, and the library exits above 4 GB. | risk |
| Code search index | Engineering | Accepted: store the scan start beside the index. An entry is racy when its times fall inside the racy window before that start. Distrust every entry when the clock is behind the start. | Comparing with now lets a file changed just after the scan look old. A backward clock step can produce stale trust. | risk |
| Code search index | DX, Engineering, Ruling | Accepted: no index on Windows. Every file is read as it is today. | The Windows attribute structs hold no change time, so the trust rule has no input there. Output stays identical. | risk |
| Code search index | Engineering | Accepted: index only the text files the engine would search, and only the workspace root. A scope inside the workspace uses that index, and a scope outside it is refused, as today. | Scopes outside the root would otherwise index a whole filesystem. | clarity |
| Code search index | Engineering | Accepted: land the differential and crash tests before the index is used. Deleting the index directory restores today's engine. | Byte identity is the contract, so its test must exist first. | sequencing |
| Code search index | DX | Accepted: pass the cache root and the racy window from the parent through the environment or an unexported parameter. Never pass them through call.Args. | The model writes call.Args, so a cache path there would let the model choose where the index lives. | risk |
| Code search index | DX | Accepted: the racy window and the 32 MiB build threshold are unexported values that tests can set. | os.Chtimes cannot age change time, and a 32 MiB fixture is slow on every run. | audience |
| Code search index | DX | Accepted: the cache file name carries the codesearch index format and a hash of the workspace root. Index failures go to the child's stderr, never to tool output. | A pinned library can change its on-disk format. Maintainers still see each fallback in the runner's durable output. | reversibility |
| Code search index | DX | Accepted: the byte-identity test compares the indexed run with the hand-written expectations in search_test.go, not with a saved output. | A saved-output pin does not count as proof. | risk |
| Code search index | Ruling | Accepted: the index lives under the product home, never in a directory that sandboxed commands can write. | The sandbox adds the temp and user cache directories to its writable roots, so a model-run command could rewrite the index. | risk |
| Code symbols | CEO, Owner | Overruled: ship the symbols mode and cut the syntax mode and its six extra grammars. The owner fixed three modes, symbols, definition and syntax, over 23 grammars. | The owner's fixed decision settles this. | impact |
| Code symbols | CEO, Owner | Overruled: hand-written queries for six languages first, and the other eleven later. The owner fixed 17 definition queries and 6 syntax-only grammars in the first release. | The owner's fixed decision settles this. | impact |
| Code symbols | CEO, DX, Owner | Overruled: symbols stays dalgona-only, and dal stays on grep. The owner decided both products get the modes, and the size cost goes into the dependency row. | The parser core adds about 17 MB, and bin/dal is 34.8 MB today. | audience |
| Code symbols | CEO, Ruling | Accepted: set the per-file size cap and the per-call time budget from a benchmark on an idle host and on a loaded host. | Parse speed is 0.1 to 0.3 MB/s on a loaded host, so a byte cap alone sets the call time. | risk |
| Code symbols | CEO | Accepted: pin gotreesitter v0.57.0. Require golden parse tests for every language on each upgrade. | The 206 of 206 parity claim is the vendor's own, so golden tests have to check it. | risk |
| Code symbols | CEO | Accepted: create THIRD_PARTY_NOTICES.md in the commit that adds the grammars, with the MIT notices of the library and each grammar. | The file does not exist at the root, and the notices must ship with the binary. | risk |
| Code symbols | Design | Accepted: one search tool with symbols, definition and syntax as modes, next to find and grep. The reviews counted four modes and left out definition, which the owner fixed. | All modes share the walk, the ignore rules and the 16 MiB ceiling, so one tool keeps the rules in one description. | clarity |
| Code symbols | Design | Accepted: pattern stays required for find, grep and syntax. It becomes optional for symbols. The other modes keep the empty-pattern error. | An outline needs no pattern. | clarity |
| Code symbols | Design | Accepted: symbol names match with RE2 and the grep flags literal and ignore_case. Anchors are refused. | A person who knows grep needs no second matching rule. | clarity |
| Code symbols | Design | Accepted: symbols rows read `path:start-end: kind first-line`. Syntax rows read `path:line: capture: text`, with no at sign. A first line over 500 characters is cut with `...`. The truncation line stays last. | The search description says the last line starts with `[Truncated:`. | clarity |
| Code symbols | Design, DX | Accepted: symbols and syntax rows carry no capture tags. | Symbol search sits outside the edit contract, and that contract refuses symbol operations. | risk |
| Code symbols | Design | Accepted: each error and empty reply names the next call. An unknown lang lists the 23 names. A language without a definition query points to syntax. A query error gives line and column. | A reply with no next step leaves the model stuck, and an empty reply can hide a language filter that removed every file. | audience |
| Code symbols | Design, Engineering | Accepted: a whole-call time budget of 10 s ends the walk with the rows already parsed and a truncation line. It is not an error. | A timeout error would discard rows already parsed. | risk |
| Code symbols | Engineering | Accepted: parse with ParseStrict under a 256 MiB memory budget, stack depth 8,192 and the work limits. | The default parse returns partial trees with a nil error, and its default budget can reach 8 GiB for a 16 MiB file. | risk |
| Code symbols | Engineering | Accepted: each file is checked against the size cap before parsing. The sandboxed child is killed at the 10 s deadline. | The library checks timeouts between steps, so only the kill is a hard stop. | risk |
| Code symbols | Engineering | Accepted: parsing runs only in the sandboxed child, never in the host process, except the rewrite tool, which parses in the host under its own bounds (Code rewrite, decision 4). Results and parse problems are separate lists. Each row names a range read in the same call, and no partial tree becomes a result. | A Go stack overflow ends the process with no recovery. | risk |
| Code symbols | Engineering, Ruling | Accepted: a depguard rule against the aggregate grammar package and the four copyleft grammars (caddy, disassembly and jq under GPL-3.0, nim under MPL-2.0). The rule also binds the rewrite tool, which imports internal/syntax. go list -deps checks the build. | The default grammar import includes the copyleft grammars, and per-grammar imports keep them out only until a rule enforces it. | risk |
| Code symbols | Engineering | Accepted: release each parse tree after its rows are read. Cap the rows of each syntax query, and mark a cut with a truncated line. | The caller must release the tree, and an uncapped query has no row limit. | risk |
| Code symbols | Engineering | Accepted: a name pattern is RE2 on the definition name. The grep prefilter uses the literal the pattern requires. A pattern with no required literal scans every file. | The plan did not say that a wildcard pattern forces a full scan. | clarity |
| Code symbols | DX | Accepted: one language table holds name, extensions, grammar import, definition query and syntax-only flag. | Grammar, query and extension map would otherwise sit in three files, and the spec list would drift. | audience |
| Code symbols | DX | Accepted: test each language against hand-written expected rows, not saved output. | A hand-written row is an oracle that the parser did not write. | risk |
| Code symbols | DX | Accepted: parse problems appear on `[Parse:` lines, a sibling of the `[Truncated:` marker. | One convention lets a model and a person find both kinds of marker. | clarity |
| Code symbols | DX | Accepted: record gotreesitter as MIT, with its copyright line from the LICENSE in the module cache. Take each grammar's licence from licenses/grammars.json. Its extra go.mod requirements are not compiled into dalgona. | The dependency rule needs the licence in the row. | risk |
| Code symbols | DX | Accepted: measure bin/dal and bin/dalgona before and after the parser core lands, and record both sizes in the dependency row. | The owner accepted the cost in both products, so the size is the number to track. | impact |
| Code symbols | CEO | Accepted: add dependency rows to docs/decisions/dependencies.md for the syntax library, its grammars, the diff library and the emulator test library before any import lands. | A new module dependency needs its row. | clarity |
| Eval default | CEO | Accepted: keep the wider default cell scope. Approval still applies to cell calls, and one test proves it before release. | A denied call halts the run with StatusDenied. | risk |
| Eval default | CEO, Ruling | Overruled: ship eval-first only after the normal and eval-first runs of the existing eval set are recorded. | The ruling keeps the drafted wiring and overrules the comparison as a gate. The code keeps the owner's eval-first default. | sequencing |
| Eval default | CEO, Owner | Accepted: dal gains the eval tool. Record dal's binary size before and after it lands. | The owner fixed that dal gains the eval tool and the tool modes only. | audience |
| Eval default | CEO | Accepted: rewrite docs/decisions/two-products.md so that minimal names what dal leaves out: plugins, workflows, hooks and dalgona-only settings. | The record says dal stays the minimal agent, and the code would otherwise contradict it. | clarity |
| Eval default | Design | Accepted on 2026-11-12: the default cell allow list includes ask, and the refusal runs before the allow check and names the tool. | The owner's rule is every tool except eval and yield. Calls that wait for a person are refused in cells, and Ask decision D10 stands. | risk |
| Eval default | Design | Accepted: keep the name eval-first. It is a config value, and a rename breaks settings files. | Renaming would break existing settings files, and the name still describes the default. | reversibility |
| Eval default | Design | Overruled on 2026-11-12: dal's eval wires only ctx.spawn and ctx.judge. | The ruling of 2026-11-12 gives dal the same ctx builtins as dalgona, because one description constant and one engine serve both products. Base.Judge is set for both products (`internal/app/app.go:81`), so judge needs no client that dal lacks. | clarity |
| Eval default | DX | Overruled on 2026-11-12: dal's eval exposes every ctx builtin that its description names. | This follows the owner's rule for dal, and it keeps the eval description true for dal. | audience |
| Eval default | Design | Overruled on 2026-11-12: dal accepts tool_mode and the [eval] table. Both leave dalgonaOnlyKeys in internal/config/merge.go. | The owner gives dal the tool modes only. | clarity |
| Eval default | DX | Accepted on 2026-11-12: dal accepts the [eval] table, whose key uses narrows what a cell may call. | The owner gives dal the eval tool and the tool modes, so the table loads in both products. | clarity |
| Eval default | Design | Overruled: no changelog exists. README and docs/spec/config.md name the new default and the setting that restores normal mode. | The first tool list the model sees changes at upgrade, and the Settings panel does not show the tool mode. | reversibility |
| Eval default | Design | Accepted: dal's eval-first adds the eval tool and folds at most the tools dal already registers. A test lists dal's visible tools in eval-first. | `toolSet` in `internal/app/app.go` registers `web_fetch` for both products, so dal folds it too. | audience |
| Eval default | DX | Accepted: flip the tool-mode default for both products in its own commit, after the commit that adds dal's eval tool. | ModelTools drops names it does not find without an error, so an earlier flip would hide todo and webfetch. | sequencing |
| Eval default | DX | Accepted: dal users reverse the default with tool_mode set to normal. The README and docs/spec/config.md state it. | Existing dal users see a different tool list after the upgrade. | reversibility |
| Eval default | DX, Engineering | Accepted: collapse DefaultUses into one rule: every tool except eval and yield, in every mode, with calls that wait for a person refused. Delete the per-mode switch in internal/script/mode.go. | The switch would return one answer for every mode, so its branches are dead code. | clarity |
| Eval default | DX | Overruled on 2026-11-12: the mode names stay in three places, because this plan adds no mode. Today the names sit in the constants, the validation list and the visibleNames switch. | A new mode would need edits in three files, and the error text could drift from the constants. | audience |
| Eval default | DX, Engineering | Overruled on 2026-11-12: no split. dal imports internal/scripthost, and the exact entry internal/script$ keeps that import allowed. Battery packages keep plugins and workflows. The eval extension carries ctx.judge, ctx.judge_batch, ctx.spawn and ctx.wait in both products. The depguard lists change in the same commit. | The dal depguard rule keeps internal/script out of dal, and the exact entry keeps battery code out of dal. | sequencing |
| Eval default | Engineering | Accepted invariants: each cell call runs under the thread's approval mode and is gated as the same direct call. Eval never calls eval. Mechanical classes stay mechanical. dal links no plugin, hook or workflow code. | These are the properties the wider default must keep. | clarity |
| Eval default | Engineering | Accepted: the wider default adds no approval bypass. It widens reach, not authority. | exec is ClassExec and patch is ClassEdit, and the gate decides both in ask and auto modes. | risk |
| Eval default | Engineering | Accepted: task stays in the cell default, and docs/spec records that it starts children without a prompt in ask mode. | ClassAgent is accepted mechanically in every approval mode, and direct calls already reach task in normal mode. | risk |
| Eval default | Engineering | Accepted: add a per-cell cap on every tool call from one cell. The value is 1024 calls per cell, fixed, counting every nested call. | No existing cap limits total gated calls in one run, and in ask mode each call is a prompt. | risk |
| Eval default | Engineering | Accepted: update the config rules, docs/spec/config.md, dalgonaOnlyKeys and docs/decisions/two-products.md in one commit, and record starlark as a new dal dependency. | Each record must say what dal now links. | clarity |
| Comment removal | CEO, Ruling | Overruled: land the comment gate first and the deletions last. | The ruling says the gate lands last. A gate that lands first fails on the comments still in the tree. | sequencing |
| Comment removal | Design, Engineering, Ruling | Accepted: the gate lands in the last commit of the workstream, after the deletion commits and after the spec-style gate joins verify. | Each order step must pass just verify. | sequencing |
| Comment removal | CEO | Accepted: delete comments one package per commit. | Deletions touch files that the terminal and edit-review workstreams also edit. | sequencing |
| Comment removal | Design | Accepted: the gate prints `<path>:<line>: comment; restructure so a name or a type says it, or move its rule to docs/spec.` Directives are allowed only in the exact forms that the comment-removal workstream lists. | A contributor can fix the line from the message alone. | clarity |
| Comment removal | CEO, Design, DX, Engineering | Overruled: the gate allows directives only in the exact forms that the comment-removal workstream lists, not by prefix. Any other comment fails, and so does a trailing comment other than a `//nolint` line with a reason. | A gate that rejects `//go:embed` breaks the build, and directives stay. | risk |
| Comment removal | CEO | Overruled: the gate also allows `export` and the generated-code header. | Generated TypeScript is exempt by path, and no Go file imports C, so neither allowance is needed. | clarity |
| Comment removal | CEO | Accepted: move knowledge into docs/spec by file and symbol name, never by line number. | Line numbers in specs drift with every edit. | clarity |
| Comment removal | CEO, DX | Accepted: delete godoc on internal declarations. Each sentence that states a rule the spec lacks moves to docs/spec first, unless the owner marks it redundant. | No outside consumer reads godoc under internal/, and the owner decided doc comments leave the source. | audience |
| Comment removal | Design, Engineering | Accepted: deleting doc comments breaks no lint rule. revive runs three named rules, and staticcheck disables ST1000 and ST1020 to ST1022. | No rule requires a doc comment, and nothing in the repository reads __doc__ or calls help(). | risk |
| Comment removal | Design, DX, Engineering | Accepted: recount comment lines per language with the gate's scanner before deletions start. The owner's Go count of 111 lines in 72 blocks, and the Svelte and Python counts, were not reproduced. | The gate's scanner gives the true list. | risk |
| Comment removal | Design, DX | Accepted: CONTRIBUTE.md line 11 names docs/spec, with a file path, as the place for the reason behind a protocol rule. It lands with the gate. | The current line invites citations that the gate would then block. | clarity |
| Comment removal | DX | Accepted: the gate uses go/scanner in ScanComments mode, because it also reads a file that does not parse. Engineering's go/parser with ParseComments is overruled. | Both are standard library. One ParseComments call returns the comment groups per file, with their positions. | clarity |
| Comment removal | DX | Accepted: move the run command and scope sentence of docs/spec/hashline/verify_model.py into docs/spec/hashline, then delete the file's comments and docstrings. | The reviews disagree on whether the index entry holds these facts. Moving them first costs nothing. | clarity |
| Comment removal | Engineering | Accepted: exempt the at least 724 generated TypeScript files, which carry DO NOT EDIT banners, from removal and from the gate. | Regeneration would restore removed banners, so the gate would fail on every regeneration. | clarity |
| Comment removal | Engineering | Accepted: the gate uses go/scanner in ScanComments mode over the Go tree, with no regex and no new dependency. | A regex on // matches text inside string literals and misses block comments. | risk |
| Comment removal | Engineering | Accepted invariant: no prose comment remains in tracked source outside generated files. The gate fails only on prose comments. | Directives are the only comments the gate allows. | clarity |

#### Open questions of the later reviews

| Area | Question | Default the plan takes |
|---|---|---|
| Terminal print path | What does dal do when stdout is not a terminal? | Exit before the program starts, with one stderr line that names the cause and a nonzero status. A test covers it. This path is separate from the failed-start row. |
| Code symbols | Should dal get the symbols and syntax modes? | Yes. The owner decided both products get the modes. The size cost is recorded in docs/decisions/syntax-package.md. |
| Code symbols | Should the syntax mode and its six extra grammars wait for a later release? | No. The owner fixed 23 grammars, 17 definition queries and 6 syntax-only grammars for the first release. |
| Code symbols | Should all 17 definition queries ship at once? | Yes. Each query ships with golden tests. |
| Code symbols | What name rule does definition mode use? | The reviews do not cover definition mode. The default is a required pattern that matches names the way symbols does: RE2, with literal and ignore_case, and no anchors. |
| Code symbols | Which whole-call time budget fits a loaded host? | 10 s per call. The per-file size cap comes from the benchmark. |
| Code search index | Should the trigram index wait for a benchmark? | The baseline benchmark lands first. The index then lands whatever the numbers are, per the ruling. |
| Code search index | Where does the index live, and how large may it grow? | The product home, which sandboxed commands cannot write. One index per search root, replaced on rebuild. Each build has a byte cap. No total cap across roots. docs/spec/search.md gives the path and says deleting the directory is safe. |
| Code search index | Which codesearch version is pinned? | Tagged v1.2.0, BSD-3-Clause. The v1.3.0-rc.1 pre-release is not pinned. |
| Edit review | Is 8 s right for up to 8 parts, 4 at a time? | Keep 8 s for both units, and measure in the held-out run. If large edits time out most often, raise the limit for parts only, in a later change. |
| Edit review | Does one wait budget cover all rounds of a turn? | No. The bound is per round. |
| Edit review | May a part that holds a cut hunk speak? | Yes. The held-out measurement decides later. |
| Edit review | Does the first release ask the 13 questions beyond sound and the six candidates? | No. |
| Edit review | Which command runs the ratchet measurement, and does it belong in verify? | A separate just recipe outside verify. It fails, not skips, when decision service credentials are missing. |
| Eval default | Should a cell default include task? | Yes, as the owner's rule says. docs/spec records that task starts children without a prompt in ask mode. |
| Eval default | What is the per-cell cap on tool calls from one cell? | 1024 tool calls per cell, counting every nested call. The value is fixed, and no measurement gates the flip. |
| Eval default | Does dal accept the [eval] table? | Yes, decided on 2026-11-12: dal accepts tool_mode and the [eval] table. |
| Eval default | Does the ask workstream's rule that cells never call ask stand? | It conflicts with the owner's rule that every tool except eval and yield is callable by default. Decided on 2026-11-12 by the owner: ask stays out of cells, and the refusal names the tool. The ask workstream's rule stands. |
| Eval default | Does the earlier pick B in the section on the default allow list of a script cell still stand? | No. Pick B says dal keeps no eval tool. The owner's fixed rule says dal gains the eval tool and the tool modes, so that section is superseded and needs a rewrite. |
| Comment removal | Keep godoc on exported names in internal packages? | Delete it. No package outside this module imports internal/. |
| Comment removal | Should the ts-rs generator stop writing banners and doc blocks? | No. Keep the generated files and exempt them from removal and from the gate. |

#### From the owner's view, later

| Work |
|---|
| Terminal (Terminal step 3): Grep the bubbletea v2.1.1 module for PrintlnRaw before the print step starts, and re-evaluate print rules 2 and 3 if it exists. |
| Terminal (Terminal step 3): On each Bubble Tea version bump, recheck issues 1736, 1881 and 1885 and PR 1741, and remove print rules 2 and 3 when a release contains the fix. |
| Terminal: State in the terminal record that the x/vt test is a regression gate and that real-terminal checks are the user gate. |
| Edit-review (Edit-review step 4): Make a failed part add no scores and keep the flags of the parts that answered, turn a clean verdict uncertain, and fail the unit only when every part fails, with a test of three answering parts and one failing. |
| Edit-review (Edit-review step 9): Keep every speaker silent until the step 9 measurement names it, and treat the built-in set as a candidate list only. |
| Edit-review: Ask only sound and the six candidate speakers in the first release. |
| Edit-review (Edit-review step 9): Measure false speech per part rather than per edit, and record per-part latency in the held-out measurement. |
| Edit-review: If a part's p95 latency exceeds 4 s, set reviewMaxParts to 4 before the feature is enabled by default. |
| Code search: Run the baseline grep benchmark on a large repository before the index work, and report the share of time spent reading files. The index lands whatever the numbers are. |
| Code symbols: Add golden parse tests for every language, and rerun them on each gotreesitter upgrade. |
| Code symbols: Create THIRD_PARTY_NOTICES.md at the repository root in the commit that adds the grammars, with the MIT notices of the library and each grammar. |
| Code symbols: Benchmark parse throughput on an idle host and on a loaded host, and set the per-file size cap and the call budget from the results. |
| Code symbols: Add dependency rows to docs/decisions/dependencies.md for the syntax library, its grammars, the diff library and the emulator test library before any import lands. |
| Eval default: Add a failing test that a cell's exec call in normal mode still asks for approval under the ask approval mode, so the wider default scope is proven safe before release. |
| Eval default: Add dal's eval tool and record dal's binary size before and after it lands. |
| Eval default: Rewrite docs/decisions/two-products.md so that minimal names what dal leaves out: plugins, workflows, hooks and dalgona-only settings. |
| Comment removal: Move each comment's fact into docs/spec by file and symbol name, with no line numbers, before its comment is deleted. |
| Comment removal: Delete comments one package per commit. |

#### From the design view, later

| Work |
|---|
| Terminal (Terminal step 3): Count only dock height, window size and AltScreen in the 50 ms settle, and state in docs/spec/tui.md that text changes never reset it. |
| Terminal (Terminal step 37): Commit the welcome row on a failed start with the error on one line, and test that the first printed row shows the error. |
| Terminal (Terminal step 15): Show the count of finished rows held by an open review in the review header. |
| Terminal (Terminal step 3): Test that rows held by an open review print once after the review closes, with no dock rows in the history. |
| Edit-review (Edit-review step 4): Give the review summary the truncated input, and print the clean text for a clean verdict with unsent hunks as the reviewed part only. |
| Edit-review (Edit-review step 4): Cut the review error text at a character boundary, measured in cells. |
| Edit-review (Edit-review step 6): Label a flag that no part speaks for as not sent to the model, and update the test that expects the old label. |
| Edit-review (Edit-review step 5): Add the coverage line to the feedback block when a speaking flag comes from a partial review. |
| Edit-review (Edit-review step 8): Print a partial clean row at verbosity high beside the uncertain rows. |
| Code search: Write the byte-identity sentence in docs/spec/search.md as one MUST sentence, and add a test that compares indexed and unindexed output on long-line, invalid-UTF-8 and case-folded corpora. |
| Code search: Start the index build after the call returns, so that no call waits for the child that reads every file. |
| Code search: Write each index into a new generation directory, sync it, and publish it by renaming CURRENT. Drop an index whose trigram size does not match. |
| Code search: Send the library crash to the host log, and test that the retry without the index returns today's output. |
| Code symbols: Add symbols, definition and syntax as modes of the search tool in both products, next to find and grep. |
| Code symbols: Make pattern optional for symbols only, and keep the empty-pattern error for the other modes. |
| Code symbols: Match symbol names with RE2 and the grep flags literal and ignore_case, and refuse anchors. |
| Code symbols: Write the row and footer grammar for symbols and syntax rows, cut first lines at 500 characters with ..., and keep the truncation line last. |
| Code symbols: Leave capture tags off symbols and syntax rows, and state that in docs/spec/search.md. |
| Code symbols: Make each error and empty reply name the next call, including the 23-name list for an unknown lang and the syntax pointer for a language with no definition query. |
| Code symbols: Add a 10 s whole-call time budget that ends the walk with the parsed rows and a truncation line, not an error. |
| Eval default: Reject the [eval] table in dal, accept tool_mode in dal, and move tool_mode out of dalgonaOnlyKeys in internal/config/merge.go while eval stays there. |
| Eval default: Add dal's eval tool with folding limited to the tools dal already registers, and add a test that lists dal's visible tools in eval-first. |
| Eval default: Add a changelog line that names the new default and the setting that restores normal mode. |
| Comment removal: Add the Python and Svelte gate as one just recipe that allows the noqa, type and pragma prefixes and svelte-ignore, and run it inside verify. |
| Comment removal: Make the gate print `<path>:<line>: comment; restructure so a name or a type says it, or move its rule to docs/spec.` |
| Comment removal: Add the comment gate as the last commit of the workstream, after the deletions and after the spec-style gate joins verify. |

#### From the developer's view, later

| Work |
|---|
| Terminal (Terminal step 3): Start the window height at 0 and add a test that no row prints before the first size message. |
| Terminal (Terminal step 3): Build the emulator and the program inside one synctest bubble, and call tea.WithoutSignals() in every program test. |
| Terminal (Terminal step 3): Rewrite the emulator test so its sleeps advance the fake clock inside the bubble and its output reads go under a mutex. |
| Terminal (Terminal step 3): Record the test-only x/vt dependency in docs/decisions/dependencies.md with its MIT licence and pinned version before the emulator test lands. |
| Terminal: When stdout is not a terminal, exit before the program starts with one stderr line that names the cause and a nonzero status, and add a test for that path. |
| Terminal: Keep the Bubble Tea godoc and its code path out of docs/spec and comments as the rule for printing under the alternate screen. |
| Edit-review (Edit-review step 4): Name the failed part in the error text as part i of n followed by the cause, and rename TestReviewPartErrorFailsUnit to a test that expects a failed part to count as not sent. |
| Edit-review (Edit-review step 4): Make a truncated edit unit never clean, so its verdict is at most uncertain. |
| Edit-review (Edit-review step 4): Take the question-set size in the packer test from reviewQuestionSet through decide.RequestSize, not from a fixed 6,100 bytes. |
| Edit-review (Edit-review step 3): Validate the edit_review feedback ids at host start, and name the key and the valid ids in the error. |
| Edit-review (Edit-review step 9): Move the speaker flag into the question table, keep ratchet.json for measured precision only, and add a test that fails when a speaker has no ratchet row. |
| Code search: Record codesearch v1.2.0 in docs/decisions/dependencies.md with its BSD-3-Clause licence and its empty requirement list before the import lands. |
| Code search: Make the byte-identity test expectations hand-written rows in search_test.go, not a saved output. |
| Code search: On Windows, read every file and trust no index entry. |
| Code search: Pass the cache root and the racy window from the parent through the environment or an unexported parameter, never through call.Args. |
| Code search: Make the racy window and the 32 MiB build threshold unexported values that tests can set. |
| Code search: Place the index under the product home, name the file with the codesearch index format and a hash of the workspace root, and check that sandboxed commands cannot write to that home. |
| Code search: Write the index sections of docs/spec/search.md: byte identity, the trust rule, the Windows rule, the racy window, the cache location, and the delete-to-restore rule. |
| Code symbols: Keep results and parse problems in separate lists, and show parse problems on [Parse: lines beside the [Truncated: marker. |
| Code symbols: Build one language table with name, extensions, grammar import, definition query and syntax-only flag, so that adding a language touches one table, one query file and one fixture. |
| Code symbols: Test each language against hand-written expected rows, not against saved output. |
| Code symbols: Pin gotreesitter v0.57.0 in docs/decisions/dependencies.md with its MIT licence, and take each grammar's licence from licenses/grammars.json. |
| Code symbols: Measure bin/dal and bin/dalgona before and after the parser core lands, and record both sizes in the dependency row. |
| Eval default: Give dal's eval every ctx builtin that its description names, so that the description stays true for dal. |
| Eval default: Flip the tool-mode default for both products in its own commit, after the commit that adds dal's eval tool. |
| Eval default: Add a test that counts the model's tools for each product under the new default, so that a missing eval tool fails the test run. |
| Eval default: Keep the tool-mode names in three places, because this plan adds no mode. |
| Eval default: Delete DefaultUses. Cells reach every tool except eval and yield in every mode, and calls that wait for a person are refused. |
| Eval default: State in the README and docs/spec/config.md that tool_mode set to normal restores the old tool list. |
| Comment removal: Replace the citation sentence in CONTRIBUTE.md line 11 with a rule that names docs/spec and a file path as the place for the reason behind a protocol rule. |
| Comment removal: Delete godoc on internal declarations only after each sentence that states a rule the spec lacks is in docs/spec. |

#### From the engineering view, later

| Work |
|---|
| Terminal (Terminal step 3): Test the print invariants with the emulator: no print in an update that changes dock height, window size or AltScreen, the dock stays at most window height minus 1, and View is empty before the first print. |
| Terminal (Terminal step 3): End the emulator's copy goroutine before the bubble returns, so synctest.Test does not wait on it. |
| Terminal (Terminal step 3): Define printedMsg as the renderer's insert call returning, and state in docs/spec/tui.md that a failed write is invisible to the client. |
| Terminal (Terminal step 3): State in the design record that rows held by an open review have no count bound. |
| Edit-review (Edit-review step 4a): Make a truncated turn unit never speak, while its verdict is still journaled. |
| Edit-review (Edit-review step 9): Replay packParts and combineParts in the held-out measurement, and count errors per part and per unit. |
| Edit-review (Edit-review step 4): Add a host-wide limit on judge requests in flight, held on the host and counted within each unit's budget. |
| Edit-review (Edit-review step 4): Cap the bytes one unit loads at 4 MiB before packing, and mark the unit truncated when the cap stops the load. |
| Edit-review (Outcome): State in docs/spec and in the Outcome that the 8 s bound applies per round. |
| Edit-review (decision record): Name the decider provider as the recipient of edit content in the edit-review decision record. |
| Edit-review (Edit-review step 4): Test the edit-review invariants with a fake judge: each request at most 30,000 bytes, at most 8 parts with 4 in flight, and no approve or reject verdict. |
| Code search: Guard each index build with a per-workspace lock, and have readers open only the final file name. |
| Code search: Quarantine an index whose child exits and back off before the next build, and test with a truncated index that the retry returns today's output. |
| Code search: Cap the bytes and memory of one build, and stop the build before codesearch's 4 GB limit. |
| Code search: Store the scan start beside the index, treat entries inside the racy window before that start as racy, and distrust every entry when the clock is behind the start. |
| Code search: Index only the text files the engine would search and only the workspace root, and read other scopes as today. |
| Code search: Land the differential and crash tests before the index is used, and document that deleting the index directory restores today's engine. |
| Code symbols: Parse with ParseStrict under a 256 MiB memory budget, stack depth 8,192 and the work limits, and never parse in the host process. |
| Code symbols: Check each file against the per-file size cap before parsing, and kill the sandboxed child at the 10 s deadline. |
| Code symbols: Add a depguard rule against the aggregate grammar package and the four copyleft grammars, and check the build with go list -deps in the commit that adds the dependency row. |
| Code symbols: Release each parse tree after its rows are read, and cap the rows of each syntax query with a truncated marker. |
| Code symbols: Compile name patterns as RE2 on the definition name, prefilter files with the literal the pattern requires, and scan every file when no literal is required. |
| Code symbols: Keep partial trees out of results, and let each row name only a range read in the same call. |
| Code rewrite: Make the structural rewrite tool import internal/syntax only through the per-grammar packages, so that the depguard rule binds the rewrite tool too. |
| Eval default: Move the Starlark runtime from internal/script into a core package before dal imports it, and change the depguard rules in the same commit. |
| Eval default: Record starlark as a new dal dependency in docs/decisions/dependencies.md. |
| Eval default: Add a cap of 1024 tool calls to each cell, counting every nested call. The value is fixed. |
| Eval default: State in docs/spec that task in a cell starts children without a prompt in ask mode. |
| Eval default: Update docs/spec/config.md, the dalgonaOnlyKeys list and docs/decisions/two-products.md in one commit, saying that dal gains eval and the tool modes. |
| Comment removal: Write the Go comment gate with go/scanner in ScanComments mode over the Go tree, allowing the four exact directive forms and failing on any other comment. |
| Comment removal: Exempt the at least 724 generated TypeScript files from removal and from the gate. |
| Comment removal: Recount comment lines per language with the gate's scanner before deletions start, and correct the plan's counts. |
| Comment removal: Move the run command and scope sentence of docs/spec/hashline/verify_model.py into docs/spec/hashline, then delete the file's comments and docstrings. |

