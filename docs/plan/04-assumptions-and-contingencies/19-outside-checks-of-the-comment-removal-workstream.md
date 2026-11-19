### Outside checks of the comment-removal workstream

- Check: a live sign-in with the `anthropic` provider and with the `antigravity` provider succeeds on a build that holds the corrected constants, before the step is committed. Guards: Comment removal step 5. Why it is outside: the sign-ins need live provider accounts and a browser consent step, and this read-only plan cannot reach them. Fallback: if either sign-in fails with the corrected value, commit nothing of that step, stop it, and report the provider and the error. The owner chose this rule on 2026-11-12, and the owner decides what happens to the constant.
- Check: `git cat-file -e f2cb47b8:engine/dal-provider/src/family/devin/proto.rs` and the same command for `request.rs` succeed in the dal repository, and the enum values in those files match the constants in `internal/model/devin/decode.go` and `internal/model/devin/request.go`. Guards: Comment removal step 13. Why it is outside: this plan's reader has no shell, so it cannot read the repository history at that commit. Fallback: if either file is absent, stop Step 13 and report it; the Devin constants stay as they are until the owner names their source.
- Check: `just hashline-model` passes on a host with `uv` and Python 3.12, and the output matches `docs/spec/hashline/verification.json`. Guards: Comment removal step 19. Why it is outside: the run needs `uv` and a Python 3.12 interpreter, which a read-only plan does not run. Fallback: if the output changes or the script fails, revert the Step 19 deletions in `verify_model.py` and report it; a skipped run counts as not run.
- Check: `.references/pi/packages/ai/src/auth/oauth/anthropic.ts` and `.references/oh-my-pi/packages/catalog/src/compat/rules/auth/google-antigravity.kdl` exist on the host that runs Step 5. Guards: Comment removal step 5. Why it is outside: `.references/` is git-ignored and absent from a clean clone. Fallback: stop Step 5 and report the missing checkout.

The lines below go to `tasks/autoplan-ledger.jsonl` in this repository when the plan is approved. The file of the same name inside the ignored `dalgona-private/` folder is not read or written. Each line is one item of the section "Work added by the reviews".

```jsonl
{"id": "CEO-01", "description": "Pins step 5: Run the OSV batch query for every row of the pins table, including go-udiff v0.4.1, and record each result in the table that Pins step 5 writes.", "phase": "CEO"}
{"id": "CEO-02", "description": "Web step 3: Add a web test that validates each `dalgona/` fixture against `internal/rpc/testdata/dalgona-schema` with ajv, and see it fail before the validation exists.", "phase": "CEO"}
{"id": "CEO-03", "description": "Ask step 3: Add `TestQuestionDeclinesWhenEveryWindowCannotAnswer` in `internal/agent`: two subscribers both answer with an error, and the result is the no-client text.", "phase": "CEO"}
{"id": "CEO-04", "description": "Terminal step 11: Add a terminal test that the tick chain runs in the state `awaiting input` and follows the motion setting.", "phase": "CEO"}
{"id": "CEO-05", "description": "Specifications step 40: Read every changed file under `docs/spec` once for active voice, present tense and the terms list before the style check becomes a gate.", "phase": "CEO"}
{"id": "CEO-06", "description": "Terminal step 10: Check with `grep -n keywordSpans internal/tui` that no local keyword matcher remains after Terminal step 10 lands.", "phase": "CEO"}
{"id": "DES-01", "description": "Terminal step 2: In the Step 2 test TestUntrustedTextIsSanitizedAtEveryEntry, add a row for command/exec stdout and stderr and a row for a fuzzyFileSearch file name that holds an escape byte, expecting the visible text in View().Content and no OSC sequence.", "phase": "Design"}
{"id": "DES-02", "description": "Terminal step 11: Add TestNoticeShowsDuringTurn to dock_test.go (Terminal step 11): with state running, the refusal text of Terminal step 6 appears in the live row, and the next key press removes it.", "phase": "Design"}
{"id": "DES-03", "description": "Terminal step 11: Add TestLiveRowBelowFortyColumns to dock_test.go (Terminal step 11): at 30 columns every live-row line has cells at most 29.", "phase": "Design"}
{"id": "DES-04", "description": "Terminal step 12: Add TestStatusBandFolderCutsAtSixteen to band_test.go (Terminal step 12): a 40-cell folder name renders in 16 cells and ends with three dots.", "phase": "Design"}
{"id": "DES-05", "description": "Terminal step 12: Add U+203A with expected width class N to TestBandGlyphsHaveNarrowWidth (Terminal step 12), so the prompt mark gets the same width check as the band glyphs.", "phase": "Design"}
{"id": "DES-06", "description": "Web steps 29 and 30: In SettingsPanel.dom.test.ts (Web step 30), expect the keyPaths status_hide and edit_review.enabled, and in ModelPanel.dom.test.ts (Web step 29), expect model_reasoning_effort.", "phase": "Design"}
{"id": "DES-07", "description": "Web step 10: In labels.test.ts (Web step 10), assert that idle gives Ready, that idle after a completed turn gives Done, and that disconnected gives Offline.", "phase": "Design"}
{"id": "DES-08", "description": "Web steps 20 and 36: In App.dom.test.ts (Web step 20), expect the document title dalgona · title · state in every state; and in session.test.ts (Web step 36), assert that an interrupted turn calls notifyAttention.", "phase": "Design"}
{"id": "DES-09", "description": "Web step 16: Add a browser check web/checks/keyword-mark.js that reads the computed background of mark.keyword and expects it transparent in each of the four concrete themes.", "phase": "Design"}
{"id": "DES-10", "description": "Terminal step 4: Add TestConfirmPanelBodyKeepsFourRows (Terminal step 4) at 24 rows: the revert panel shows its title, four body lines and one action line.", "phase": "Design"}
{"id": "DES-11", "description": "Terminal step 22: Add TestPopupStaysOpenOnFirstToken to complete_test.go (Terminal step 22): a slash typed at the start of a line with text after it keeps the popup open and leaves the cursor in the first token.", "phase": "Design"}
{"id": "DES-12", "description": "Terminal step 31: Add TestImagesOverTenMiBRefused (Terminal step 31): two 6 MiB images in one prompt are refused by the 10 MiB request total, with the notice text of the image-input workstream.", "phase": "Design"}
{"id": "DX-01", "description": "Live-debug step 4: After live-debug step 4 writes docs/spec/live-debug.md, run `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/live-debug.md`, fix every finding without changing a literal, and rerun until it prints `style findings: 0`.", "phase": "DX"}
{"id": "DX-02", "description": "Specifications steps 8, 14 and 16: Before each of these steps applies a row that the specification review did not check against the code, read the code line that the row cites and stop when the code differs. The rows are every row of step 8, rows 63, 153, 345, 370, 426, 457, 496 and 527 to 545 of step 14, and rows 71, 76, 82, 88, 101 and 117 of step 16.", "phase": "DX"}
{"id": "DX-03", "description": "Specifications step 24: In `docs/spec/protocol.md` line 22, replace `The merge MUST keep order.` with `The merge MUST keep the order of the text of that item. A merged delta MAY reach the client before a message that was queued between the two deltas.` The code is `internal/rpc/conn.go:566-576`.", "phase": "DX"}
{"id": "ENG-01", "description": "Edit-review step 4: Move the diff, masking and state encoding of judgeInput into the goroutine that startReview starts, set w.eligible after group.Wait in joinReviews, and add a test in internal/agent/review_test.go that blocks the masker and asserts tool.completed is journaled before the masker returns.", "phase": "Engineering"}
{"id": "ENG-02", "description": "Edit-review step 4: Start the 8 s timeout context before the factory call in askJudge (internal/agent/review.go) and add TestEditReviewFactoryHangFailsOpen, where a factory that blocks until its context ends gives an error verdict with the text review timed out after 50ms within 2 s.", "phase": "Engineering"}
{"id": "ENG-03", "description": "Edit-review step 4a: Build turn skipped paths from the capped paths list in internal/agent/review.go and add TestTurnJudgedSkippedWithinPathCap in internal/agent/event/turn_judged_test.go with 70 changed paths, where the 65th has no post blob, expecting Validate to pass and skipped to omit that path.", "phase": "Engineering"}
{"id": "ENG-04", "description": "Edit-review step 4: Add TestJudgedPathsWithheldWhenMaskingFails in internal/agent/review_test.go, where a failing masker makes tool.judged.paths equal the single entry [paths withheld: masking failed] and no journal byte holds the raw path.", "phase": "Engineering"}
{"id": "ENG-05", "description": "Edit-review step 4a: Add TestTurnReviewMasksPaths in internal/agent/review_turn_test.go, where a secret in a changed path appears in turn.judged.paths only as its placeholder and not in the journal bytes.", "phase": "Engineering"}
{"id": "ENG-06", "description": "Core-fixes step 5: Add TestAutoCompactionDoesNotRepeatSummarizer in internal/agent/compaction_test.go, where two steps of one turn with a non-shrinking summary call the summarizer once.", "phase": "Engineering"}
{"id": "ENG-07", "description": "Protocol step 13: Give asktool.Question a field that suppresses the Other row, set it for the sandbox question in internal/agent/sandbox_choice.go, and add TestSandboxQuestionHasNoOtherRow in internal/rpc/serve_input_test.go that asserts isOther is false on the wire.", "phase": "Engineering"}
{"id": "ENG-08", "description": "Protocol step 13: Make concurrent sandbox fallbacks against one parent share one pending Ask in internal/agent/sandbox_choice.go, and add TestSiblingsShareOneSandboxQuestion where two child exec calls see one question id.", "phase": "Engineering"}
{"id": "ENG-09", "description": "Image-input step 2: Add TestImagePathRejectsFIFO in internal/rpc/serve_image_test.go (skipped on Windows) that sends turn/start with a FIFO path and expects -32602 with image path must be a regular file within one second.", "phase": "Engineering"}
{"id": "ENG-10", "description": "Ask step 2: Add TestAskQuestionTextIsMasked in internal/agent/ask_tool_test.go, where a secret in a question header or option label reaches the subscriber only as its placeholder.", "phase": "Engineering"}
{"id": "ENG-11", "description": "Image-input step 5: Cache each verified image blob on the transcript in internal/agent/image.go and add TestImageBlobReadOncePerThread that counts journal reads across two model steps.", "phase": "Engineering"}
{"id": "ENG-12", "description": "Ratchet step 7: Make recovery of an open ratchet approve complete as interrupted through the first-statement check in recoverStarted (internal/agent/recover.go), and add TestRecoverInterruptsOpenRatchetApprove that expects ok=false and no new question.", "phase": "Engineering"}
{"id": "ENG-13", "description": "Protocol step 12: Verify whether ProviderModels reads the stored credential in internal/agent/access.go; if it does, pass the candidate key and call SetAPIKey only after success, and add a test in internal/rpc that leaves the store byte-identical after a failed verification.", "phase": "Engineering"}
{"id": "ENG-14", "description": "Protocol step 19: Keep each root's file list for 5 seconds in internal/rpc/serve_fuzzy.go and add TestFuzzyFileSearchReusesWalk that counts walks over two requests in the same window.", "phase": "Engineering"}
{"id": "ENG-15", "description": "Protocol step 18: Add a total stat budget across watches to the polling loop in internal/rpc/serve_fs.go and add TestFsWatchBudgetAcrossWatches that checks the per-poll entry count.", "phase": "Engineering"}
{"id": "ENG-16", "description": "Terminal step 3: Add the print tests of the terminal print rules: the view stays frozen while a print is in flight, a print waits for a settled frame, and the dock leaves one row of room.", "phase": "Engineering"}
{"id": "ENG-17", "description": "Terminal step 3: State the five print rules in `docs/spec/tui.md`, next to the width rule.", "phase": "Engineering"}
{"id": "DX-04", "description": "Terminal (Terminal step 3): Start the window height at 0 and add a test that no row prints before the first size message.", "phase": "DX"}
{"id": "DES-13", "description": "Terminal (Terminal step 3): Count only dock height, window size and AltScreen in the 50 ms settle, and state in docs/spec/tui.md that text changes never reset it.", "phase": "Design"}
{"id": "DES-14", "description": "Terminal (Terminal step 37): Commit the welcome row on a failed start with the error on one line, and test that the first printed row shows the error.", "phase": "Design"}
{"id": "DES-15", "description": "Terminal (Terminal step 15): Show the count of finished rows held by an open review in the review header.", "phase": "Design"}
{"id": "DES-16", "description": "Terminal (Terminal step 3): Test that rows held by an open review print once after the review closes, with no dock rows in the history.", "phase": "Design"}
{"id": "ENG-18", "description": "Terminal (Terminal step 3): Test the print invariants with the emulator: no print in an update that changes dock height, window size or AltScreen, the dock stays at most window height minus 1, and View is empty before the first print.", "phase": "Engineering"}
{"id": "DX-05", "description": "Terminal (Terminal step 3): Build the emulator and the program inside one synctest bubble, and call tea.WithoutSignals() in every program test.", "phase": "DX"}
{"id": "DX-06", "description": "Terminal (Terminal step 3): Rewrite the emulator test so its sleeps advance the fake clock inside the bubble and its output reads go under a mutex.", "phase": "DX"}
{"id": "ENG-19", "description": "Terminal (Terminal step 3): End the emulator's copy goroutine before the bubble returns, so synctest.Test does not wait on it.", "phase": "Engineering"}
{"id": "ENG-20", "description": "Terminal (Terminal step 3): Define printedMsg as the renderer's insert call returning, and state in docs/spec/tui.md that a failed write is invisible to the client.", "phase": "Engineering"}
{"id": "ENG-21", "description": "Terminal (Terminal step 3): State in the design record that rows held by an open review have no count bound.", "phase": "Engineering"}
{"id": "DX-07", "description": "Terminal (Terminal step 3): Record the test-only x/vt dependency in docs/decisions/dependencies.md with its MIT licence and pinned version before the emulator test lands.", "phase": "DX"}
{"id": "CEO-07", "description": "Terminal (Terminal step 3): Grep the bubbletea v2.1.1 module for PrintlnRaw before the print step starts, and re-evaluate print rules 2 and 3 if it exists.", "phase": "CEO"}
{"id": "CEO-08", "description": "Terminal (Terminal step 3): On each Bubble Tea version bump, recheck issues 1736, 1881 and 1885 and PR 1741, and remove print rules 2 and 3 when a release contains the fix.", "phase": "CEO"}
{"id": "DX-08", "description": "Terminal: When stdout is not a terminal, exit before the program starts with one stderr line that names the cause and a nonzero status, and add a test for that path.", "phase": "DX"}
{"id": "DX-09", "description": "Terminal: Keep the Bubble Tea godoc and its code path out of docs/spec and comments as the rule for printing under the alternate screen.", "phase": "DX"}
{"id": "CEO-09", "description": "Terminal: State in the terminal record that the x/vt test is a regression gate and that real-terminal checks are the user gate.", "phase": "CEO"}
{"id": "DX-10", "description": "Edit-review (Edit-review step 4): Name the failed part in the error text as part i of n followed by the cause, and rename TestReviewPartErrorFailsUnit to a test that expects a failed part to count as not sent.", "phase": "DX"}
{"id": "CEO-10", "description": "Edit-review (Edit-review step 4): Make a failed part add no scores and keep the flags of the parts that answered, turn a clean verdict uncertain, and fail the unit only when every part fails, with a test of three answering parts and one failing.", "phase": "CEO"}
{"id": "DES-17", "description": "Edit-review (Edit-review step 4): Give the review summary the truncated input, and print the clean text for a clean verdict with unsent hunks as the reviewed part only.", "phase": "Design"}
{"id": "DX-11", "description": "Edit-review (Edit-review step 4): Make a truncated edit unit never clean, so its verdict is at most uncertain.", "phase": "DX"}
{"id": "ENG-22", "description": "Edit-review (Edit-review step 4a): Make a truncated turn unit never speak, while its verdict is still journaled.", "phase": "Engineering"}
{"id": "DES-18", "description": "Edit-review (Edit-review step 4): Cut the review error text at a character boundary, measured in cells.", "phase": "Design"}
{"id": "DES-19", "description": "Edit-review (Edit-review step 6): Label a flag that no part speaks for as not sent to the model, and update the test that expects the old label.", "phase": "Design"}
{"id": "DES-20", "description": "Edit-review (Edit-review step 5): Add the coverage line to the feedback block when a speaking flag comes from a partial review.", "phase": "Design"}
{"id": "DES-21", "description": "Edit-review (Edit-review step 8): Print a partial clean row at verbosity high beside the uncertain rows.", "phase": "Design"}
{"id": "DX-12", "description": "Edit-review (Edit-review step 4): Take the question-set size in the packer test from reviewQuestionSet through decide.RequestSize, not from a fixed 6,100 bytes.", "phase": "DX"}
{"id": "DX-13", "description": "Edit-review (Edit-review step 3): Validate the edit_review feedback ids at host start, and name the key and the valid ids in the error.", "phase": "DX"}
{"id": "CEO-11", "description": "Edit-review (Edit-review step 9): Keep every speaker silent until the step 9 measurement names it, and treat the built-in set as a candidate list only.", "phase": "CEO"}
{"id": "DX-14", "description": "Edit-review (Edit-review step 9): Move the speaker flag into the question table, keep ratchet.json for measured precision only, and add a test that fails when a speaker has no ratchet row.", "phase": "DX"}
{"id": "CEO-12", "description": "Edit-review: Ask only sound and the six candidate speakers in the first release.", "phase": "CEO"}
{"id": "CEO-13", "description": "Edit-review (Edit-review step 9): Measure false speech per part rather than per edit, and record per-part latency in the held-out measurement.", "phase": "CEO"}
{"id": "ENG-23", "description": "Edit-review (Edit-review step 9): Replay packParts and combineParts in the held-out measurement, and count errors per part and per unit.", "phase": "Engineering"}
{"id": "CEO-14", "description": "Edit-review: If a part's p95 latency exceeds 4 s, set reviewMaxParts to 4 before the feature is enabled by default.", "phase": "CEO"}
{"id": "ENG-24", "description": "Edit-review (Edit-review step 4): Add a host-wide limit on judge requests in flight, held on the host and counted within each unit's budget.", "phase": "Engineering"}
{"id": "ENG-25", "description": "Edit-review (Edit-review step 4): Cap the bytes one unit loads at 4 MiB before packing, and mark the unit truncated when the cap stops the load.", "phase": "Engineering"}
{"id": "ENG-26", "description": "Edit-review (Outcome): State in docs/spec and in the Outcome that the 8 s bound applies per round.", "phase": "Engineering"}
{"id": "ENG-27", "description": "Edit-review (decision record): Name the decider provider as the recipient of edit content in the edit-review decision record.", "phase": "Engineering"}
{"id": "ENG-28", "description": "Edit-review (Edit-review step 4): Test the edit-review invariants with a fake judge: each request at most 30,000 bytes, at most 8 parts with 4 in flight, and no approve or reject verdict.", "phase": "Engineering"}
{"id": "CEO-15", "description": "Code search: Run the baseline grep benchmark on a large repository before the index work, and report the share of time spent reading files. The index lands whatever the numbers are.", "phase": "CEO"}
{"id": "DX-15", "description": "Code search: Record codesearch v1.2.0 in docs/decisions/dependencies.md with its BSD-3-Clause licence and its empty requirement list before the import lands.", "phase": "DX"}
{"id": "DES-22", "description": "Code search: Write the byte-identity sentence in docs/spec/search.md as one MUST sentence, and add a test that compares indexed and unindexed output on long-line, invalid-UTF-8 and case-folded corpora.", "phase": "Design"}
{"id": "DX-16", "description": "Code search: Make the byte-identity test expectations hand-written rows in search_test.go, not a saved output.", "phase": "DX"}
{"id": "DES-23", "description": "Code search: Start the index build after the call returns, so that no call waits for the child that reads every file.", "phase": "Design"}
{"id": "DES-24", "description": "Code search: Write the index through a temporary file that is synced, renamed into place and checksummed, and drop an index whose checksum does not match.", "phase": "Design"}
{"id": "ENG-29", "description": "Code search: Guard each index build with a per-workspace lock, and have readers open only the final file name.", "phase": "Engineering"}
{"id": "DES-25", "description": "Code search: Send the library crash to the host log, and test that the retry without the index returns today's output.", "phase": "Design"}
{"id": "ENG-30", "description": "Code search: Quarantine an index whose child exits and back off before the next build, and test with a truncated index that the retry returns today's output.", "phase": "Engineering"}
{"id": "ENG-31", "description": "Code search: Cap the bytes and memory of one build, and stop the build before codesearch's 4 GB limit.", "phase": "Engineering"}
{"id": "ENG-32", "description": "Code search: Store the scan start beside the index, treat entries inside the racy window before that start as racy, and distrust every entry when the clock is behind the start.", "phase": "Engineering"}
{"id": "DX-17", "description": "Code search: On Windows, read every file and trust no index entry.", "phase": "DX"}
{"id": "ENG-33", "description": "Code search: Index only the text files the engine would search and only the workspace root, and read other scopes as today.", "phase": "Engineering"}
{"id": "DX-18", "description": "Code search: Pass the cache root and the racy window from the parent through the environment or an unexported parameter, never through call.Args.", "phase": "DX"}
{"id": "DX-19", "description": "Code search: Make the racy window and the 32 MiB build threshold unexported values that tests can set.", "phase": "DX"}
{"id": "DX-20", "description": "Code search: Place the index under the product home, name the file with the codesearch index format and a hash of the workspace root, and check that sandboxed commands cannot write to that home.", "phase": "DX"}
{"id": "ENG-34", "description": "Code search: Land the differential and crash tests before the index is used, and document that deleting the index directory restores today's engine.", "phase": "Engineering"}
{"id": "DX-21", "description": "Code search: Write the index sections of docs/spec/search.md: byte identity, the trust rule, the Windows rule, the racy window, the cache location, and the delete-to-restore rule.", "phase": "DX"}
{"id": "DES-26", "description": "Code symbols: Add symbols, definition and syntax as modes of the search tool in both products, next to find and grep.", "phase": "Design"}
{"id": "DES-27", "description": "Code symbols: Make pattern optional for symbols only, and keep the empty-pattern error for the other modes.", "phase": "Design"}
{"id": "DES-28", "description": "Code symbols: Match symbol names with RE2, unanchored, with the grep flags literal and ignore_case.", "phase": "Design"}
{"id": "DES-29", "description": "Code symbols: Write the row and footer grammar for symbols and syntax rows, cut first lines at 500 characters with ..., and keep the truncation line last.", "phase": "Design"}
{"id": "DES-30", "description": "Code symbols: Leave capture tags off symbols and syntax rows, and state that in docs/spec/search.md.", "phase": "Design"}
{"id": "DES-31", "description": "Code symbols: Make each error and empty reply name the next call, including the 23-name list for an unknown lang and the syntax pointer for a language with no definition query.", "phase": "Design"}
{"id": "DES-32", "description": "Code symbols: Add a 10 s whole-call time budget that ends the walk with the parsed rows and a truncation line, not an error.", "phase": "Design"}
{"id": "ENG-35", "description": "Code symbols: Parse with ParseStrict under a 256 MiB memory budget, stack depth 8,192 and the work limits, and never parse in the host process.", "phase": "Engineering"}
{"id": "ENG-36", "description": "Code symbols: Check each file against the per-file size cap before parsing, and kill the sandboxed child at the 10 s deadline.", "phase": "Engineering"}
{"id": "ENG-37", "description": "Code symbols: Add a depguard rule against the aggregate grammar package and the four copyleft grammars, and check the build with go list -deps in the commit that adds the dependency row.", "phase": "Engineering"}
{"id": "ENG-38", "description": "Code symbols: Release each parse tree after its rows are read, and cap the rows of each syntax query with a truncated marker.", "phase": "Engineering"}
{"id": "ENG-39", "description": "Code symbols: Compile name patterns as RE2 on the definition name, prefilter files with the literal the pattern requires, and scan every file when no literal is required.", "phase": "Engineering"}
{"id": "DX-22", "description": "Code symbols: Keep results and parse problems in separate lists, and show parse problems on [Parse: lines beside the [Truncated: marker.", "phase": "DX"}
{"id": "ENG-40", "description": "Code symbols: Keep partial trees out of results, and let each row name only a range read in the same call.", "phase": "Engineering"}
{"id": "DX-23", "description": "Code symbols: Build one language table with name, extensions, grammar import, definition query and syntax-only flag, so that adding a language touches one table, one query file and one fixture.", "phase": "DX"}
{"id": "DX-24", "description": "Code symbols: Test each language against hand-written expected rows, not against saved output.", "phase": "DX"}
{"id": "CEO-16", "description": "Code symbols: Add golden parse tests for every language, and rerun them on each gotreesitter upgrade.", "phase": "CEO"}
{"id": "DX-25", "description": "Code symbols: Pin gotreesitter v0.57.0 in docs/decisions/dependencies.md with its MIT licence, and take each grammar's licence from licenses/grammars.json.", "phase": "DX"}
{"id": "CEO-17", "description": "Code symbols: Create THIRD_PARTY_NOTICES.md at the repository root in the commit that adds the grammars, with the MIT notices of the library and each grammar.", "phase": "CEO"}
{"id": "CEO-18", "description": "Code symbols: Benchmark parse throughput on an idle host and on a loaded host, and set the per-file size cap and the call budget from the results.", "phase": "CEO"}
{"id": "DX-26", "description": "Code symbols: Measure bin/dal and bin/dalgona before and after the parser core lands, and record both sizes in the dependency row.", "phase": "DX"}
{"id": "CEO-19", "description": "Code symbols: Add dependency rows to docs/decisions/dependencies.md for the syntax library, its grammars, the diff library and the emulator test library before any import lands.", "phase": "CEO"}
{"id": "ENG-41", "description": "Code rewrite: Make the structural rewrite tool import internal/syntax only through the per-grammar packages, so that the depguard rule binds the rewrite tool too.", "phase": "Engineering"}
{"id": "CEO-20", "description": "Eval default: Add a failing test that a cell's exec call in normal mode still asks for approval under the ask approval mode, so the wider default scope is proven safe before release.", "phase": "CEO"}
{"id": "CEO-21", "description": "Eval default: Add dal's eval tool and record dal's binary size before and after it lands.", "phase": "CEO"}
{"id": "CEO-22", "description": "Eval default: Rewrite docs/decisions/two-products.md so that minimal names what dal leaves out: plugins, workflows, hooks and dalgona-only settings.", "phase": "CEO"}
{"id": "ENG-42", "description": "Eval default: Move the Starlark runtime from internal/script into a core package before dal imports it, and change the depguard rules in the same commit.", "phase": "Engineering"}
{"id": "ENG-43", "description": "Eval default: Record starlark as a new dal dependency in docs/decisions/dependencies.md.", "phase": "Engineering"}
{"id": "DX-27", "description": "Eval default: Give dal's eval the builtins tools, try_call, scope, log and sleep, and generate dal's eval description from that set so that it names no other builtin.", "phase": "DX"}
{"id": "DES-33", "description": "Eval default: Reject the [eval] table in dal, accept tool_mode in dal, and move tool_mode out of dalgonaOnlyKeys in internal/config/merge.go while eval stays there.", "phase": "Design"}
{"id": "DX-28", "description": "Eval default: Flip the tool-mode default for both products in the same commit that adds dal's eval tool, and not before.", "phase": "DX"}
{"id": "DES-34", "description": "Eval default: Add dal's eval tool with folding limited to the tools dal already registers, and add a test that lists dal's visible tools in eval-first.", "phase": "Design"}
{"id": "DX-29", "description": "Eval default: Add a test that counts the model's tools for each product under the new default, so that a missing eval tool fails the test run.", "phase": "DX"}
{"id": "DX-30", "description": "Eval default: Derive the tool-mode validation list from one table used by the constants, the validation and the visibleNames switch in internal/script/mode.go.", "phase": "DX"}
{"id": "DX-31", "description": "Eval default: Collapse DefaultUses into one rule that returns every tool except eval for every mode, and delete the per-mode switch in internal/script/mode.go.", "phase": "DX"}
{"id": "ENG-44", "description": "Eval default: Add a per-run cap on gated calls from one cell, with the value set by measurement before release.", "phase": "Engineering"}
{"id": "ENG-45", "description": "Eval default: State in docs/spec that task in a cell starts children without a prompt in ask mode.", "phase": "Engineering"}
{"id": "ENG-46", "description": "Eval default: Update docs/spec/config.md, the dalgonaOnlyKeys list and docs/decisions/two-products.md in one commit, saying that dal gains eval and the tool modes.", "phase": "Engineering"}
{"id": "DES-35", "description": "Eval default: Add a changelog line that names the new default and the setting that restores normal mode.", "phase": "Design"}
{"id": "DX-32", "description": "Eval default: State in the README and docs/spec/config.md that tool_mode set to normal restores the old tool list.", "phase": "DX"}
{"id": "ENG-47", "description": "Comment removal: Write the Go comment gate with go/parser and ParseComments over the Go tree, allowing the directive prefixes and failing on any other comment and on any trailing comment.", "phase": "Engineering"}
{"id": "DES-36", "description": "Comment removal: Add the Python and Svelte gate as one just recipe that allows the noqa, type and pragma prefixes and svelte-ignore, and run it inside verify.", "phase": "Design"}
{"id": "DES-37", "description": "Comment removal: Make the gate print path:line: comment. Move its fact to docs/spec or delete it. Directives are allowed.", "phase": "Design"}
{"id": "ENG-48", "description": "Comment removal: Exempt the at least 724 generated TypeScript files from removal and from the gate.", "phase": "Engineering"}
{"id": "ENG-49", "description": "Comment removal: Recount comment lines per language with the gate's scanner before deletions start, and correct the plan's counts.", "phase": "Engineering"}
{"id": "CEO-23", "description": "Comment removal: Move each comment's fact into docs/spec by file and symbol name, with no line numbers, before its comment is deleted.", "phase": "CEO"}
{"id": "CEO-24", "description": "Comment removal: Delete comments one package per commit.", "phase": "CEO"}
{"id": "ENG-50", "description": "Comment removal: Move the run command and scope sentence of docs/spec/hashline/verify_model.py into docs/spec/hashline, then delete the file's comments and docstrings.", "phase": "Engineering"}
{"id": "DX-33", "description": "Comment removal: Replace the citation sentence in CONTRIBUTE.md line 11 with a rule that names docs/spec and a file path as the place for the reason behind a protocol rule.", "phase": "DX"}
{"id": "DES-38", "description": "Comment removal: Add the comment gate as the last commit of the workstream, after the deletions and after the spec-style gate joins verify.", "phase": "Design"}
{"id": "DX-34", "description": "Comment removal: Delete godoc on internal declarations only after each sentence that states a rule the spec lacks is in docs/spec.", "phase": "DX"}
{"id": "ENG-51", "description": "Let cells reach every tool except eval and yield, refuse calls that wait for a person, and stop a cell at 1024 calls.", "phase": "Engineering"}
{"id": "DX-35", "description": "Give dal the eval tool, the tool_mode key, and the [eval] table, with an exact depguard entry for internal/script.", "phase": "DX"}
{"id": "CEO-25", "description": "Make eval-first the default for both products after dal has the eval tool.", "phase": "CEO"}
{"id": "ENG-52", "description": "Add ratchet to the eval-first direct list in Ratchet step 7, and keep approve out of cells.", "phase": "Engineering"}
{"id": "DX-36", "description": "Name the eval call for task in the ratchet keyword notice.", "phase": "DX"}
{"id": "DES-39", "description": "Route the scripted t-plan and t-sub turns through eval cells, keeping the same expected observations for T14, W11, T15, and W12.", "phase": "Design"}
{"id": "DX-37", "description": "Update README and the config, starlark, and two-products specs for the new reach, the new default, and the setting that restores normal mode.", "phase": "DX"}
{"id": "ENG-53", "description": "Correct the review ledger rows that the owner's ruling of 2026-11-12 overrides.", "phase": "Engineering"}
{"id": "CEO-26", "description": "Record bin/dal size before and after the dal eval step in its commit message.", "phase": "CEO"}
{"id": "ENG-54", "description": "Add the baseline benchmarks of the scan path, so the index steps have a measured starting point.", "phase": "Engineering"}
{"id": "ENG-55", "description": "Add the manifest file format with its round-trip, rejection and lookup tests, so the index file can be checked before any build writes one.", "phase": "Engineering"}
{"id": "ENG-56", "description": "Add the index builder, the publish rules, the hidden __search-index command in both products, the codesearch dependency and its BSD-3-Clause notice, so one generation can be built and published without the search using it yet.", "phase": "Engineering"}
{"id": "DX-38", "description": "Record the codesearch Decision cell in docs/decisions/dependencies.md and create docs/decisions/search-index.md with its first sections, so the dependency and the design have one written home.", "phase": "DX"}
{"id": "ENG-57", "description": "Add the trigram query, the freshness rule, the counted-bytes rule and exit status 1, with the property test that checks indexed output against the scan.", "phase": "Engineering"}
{"id": "ENG-58", "description": "Make the parent use the index with the four gating conditions, and retry once without it after exit status 1, with fresh process ids.", "phase": "Engineering"}
{"id": "ENG-59", "description": "Start one background build after the uncovered-bytes threshold, sandboxed with its generation directory as the only extra writable root, and publish only after exit status 0.", "phase": "Engineering"}
{"id": "DES-40", "description": "Write the ## Index sentences of docs/spec/search.md in the same commits as the behaviour they describe, and check each sentence with just spec-style once it exists.", "phase": "Design"}
{"id": "DES-41", "description": "Add the sentences about index builds to docs/spec/recovery.md, so that recovery of a grep call is stated and a crash during a build is covered.", "phase": "Design"}
{"id": "CEO-27", "description": "Run the baseline and index benchmarks on a loaded host and record the rejected_files count, so the floor of decision 26 is measured for this workspace.", "phase": "CEO"}
{"id": "CEO-28", "description": "Run the threshold sweep on the 4 GB corpus on an idle host, and set indexRebuildBytes by the rule of decision 27, so the build trigger matches measured numbers.", "phase": "CEO"}
{"id": "ENG-60", "description": "Run the macOS sandbox checks and one live dal session, so the index is shown to work outside the Linux test host.", "phase": "Engineering"}
{"id": "ENG-61", "description": "Add the syntax package boundary, the Go definitions, the adapter for the token-source factories, and the depguard rules for the library and the copyleft grammars.", "phase": "Engineering"}
{"id": "ENG-62", "description": "Add the definition queries and fixtures for TypeScript, TSX, JavaScript, Python and Rust.", "phase": "Engineering"}
{"id": "ENG-63", "description": "Add the definition queries, fixtures and hostile-input tests for Java, C, C++, C# and Kotlin.", "phase": "Engineering"}
{"id": "ENG-64", "description": "Add the definition queries and fixtures for Swift, Ruby, PHP, Bash, Lua and Markdown.", "phase": "Engineering"}
{"id": "ENG-65", "description": "Add the six syntax-only grammars with their sample files and the extension test for every row.", "phase": "Engineering"}
{"id": "DX-39", "description": "Add the symbols mode of search, with its bounds, footers and the Symbols and syntax specification section, so that agents can list definitions.", "phase": "DX"}
{"id": "DX-40", "description": "Add the definition mode of search, so that agents can read one definition body by name, with a citable capture.", "phase": "DX"}
{"id": "DX-41", "description": "Add the syntax mode of search, so that agents can run a tree-sitter query with a lang argument.", "phase": "DX"}
{"id": "ENG-66", "description": "Measure parse throughput, first-use cost, narrowing and worker count, and record the bounds, the memory peak and the binary sizes.", "phase": "Engineering"}
{"id": "ENG-67", "description": "Write the THIRD_PARTY_NOTICES sections for gotreesitter, the go grammar and the other 22 grammars, copied from their licence files.", "phase": "Engineering"}
{"id": "CEO-29", "description": "Record the cost of linking the 23 grammars into dal and dalgona, since the owner accepted that cost.", "phase": "CEO"}
{"id": "DX-42", "description": "Keep the tool description, the mode names and the error texts identical in both products, so that the agents see one vocabulary.", "phase": "DX"}
{"id": "DX-43", "description": "Correct the this plan wording that counts search modes and that states the name rule as unanchored.", "phase": "DX"}
{"id": "ENG-68", "description": "Add the syntax facts that the rewrite needs: the error line of a parse, the sorted capture names of a query and the extensions of a language.", "phase": "Engineering"}
{"id": "ENG-69", "description": "Share one candidate selection between the search syntax modes and the rewrite tool, with the ignore, symlink and glob rules of search.", "phase": "Engineering"}
{"id": "ENG-70", "description": "Add `patch.PlanReplace` so that the rewrite writes through the stale guard, the journal and the rollback of patch, with the 64 MiB cap kept.", "phase": "Engineering"}
{"id": "DX-44", "description": "Add the replacement grammar, the capture grouping and the pure edit functions, with a grammar that cannot absorb text after a capture.", "phase": "DX"}
{"id": "DX-45", "description": "Add the `rewrite` tool with its bounds, its refusals, its registration in both products, its eval reach and its specification page, so that agents can rename code by structure.", "phase": "DX"}
{"id": "DES-42", "description": "Test the user-visible approval view, the completed file-change item and the pre-image journal of a rewrite call.", "phase": "Design"}
{"id": "CEO-30", "description": "Owner decision: the syntax preview cap (256 KiB) and the rewrite cap (1 MiB) differ. Decide which cap governs the preview footer.", "phase": "CEO"}
{"id": "CEO-31", "description": "Owner decision: the owner's rule for read-only calls names a sandbox mode that does not exist at commit 3c4b6f94. Confirm that the rewrite follows `patch` with no refusal, or add a read-only mode to the sandbox workstream first.", "phase": "CEO"}
{"id": "DES-43", "description": "Live check of the ask-mode approval view in a running daemon, which this plan cannot run.", "phase": "Design"}
{"id": "DX-46", "description": "Rewrite the comment rule in CONTRIBUTE.md line 11, add the one named gate exception, and point REVIEW.md line 7 at it.", "phase": "DX"}
{"id": "ENG-72", "description": "Delete the remote package comments and correct the Archive order and actor in docs/spec/runners.md.", "phase": "Engineering"}
{"id": "ENG-73", "description": "Delete the agent package comments, rename rules.CheckValue to CheckRemoteValue, and add the decider and workspace-trust sentences.", "phase": "Engineering"}
{"id": "CEO-32", "description": "Delete the app package comments and move the internal/app command-line sentences into docs/spec/protocol.md.", "phase": "CEO"}
{"id": "ENG-74", "description": "Correct the anthropicPublicID and antigravityPublicKey constants from their donor files and add the donor decoding test.", "phase": "Engineering"}
{"id": "CEO-33", "description": "Delete the auth comments and add the credential-write and OAuth citation sentences to daemon.md and oauth.md.", "phase": "CEO"}
{"id": "CEO-34", "description": "Delete the Codex app-server comments and add the --config, --listen and Origin sentences to daemon.md.", "phase": "CEO"}
{"id": "ENG-75", "description": "Delete the decide comment and make the existing decide sentence name answers as well as criteria.", "phase": "Engineering"}
{"id": "CEO-35", "description": "Delete the MCP comments and add the transport, DELETE and tool-discovery sentences to mcp.md.", "phase": "CEO"}
{"id": "ENG-76", "description": "Delete the Plan.Discard comment and add the declined-plan sentence to edit-styles.md.", "phase": "Engineering"}
{"id": "CEO-36", "description": "Delete the Antigravity comments and add the four provider sentences to providers.md.", "phase": "CEO"}
{"id": "CEO-37", "description": "Delete the Cursor comments and add the Connect, senpi and protobuf sentences to providers.md.", "phase": "CEO"}
{"id": "CEO-38", "description": "Delete the Devin comments and add the Connect and donor-commit sentences to providers.md.", "phase": "CEO"}
{"id": "CEO-39", "description": "Delete the Anthropic model-list comment and add its sentence to providers.md.", "phase": "CEO"}
{"id": "CEO-40", "description": "Delete the chat-compatible model-list comment and add its sentence to providers.md.", "phase": "CEO"}
{"id": "CEO-41", "description": "Delete the ChatGPT comments and add the base-URL and Codex-list sentences to providers.md.", "phase": "CEO"}
{"id": "CEO-42", "description": "Delete the model presets comment and add its sentence to providers.md.", "phase": "CEO"}
{"id": "ENG-77", "description": "Delete the runner comments and add the Wait sentence to runners.md.", "phase": "Engineering"}
{"id": "DES-44", "description": "Delete the hashline model checker comments and docstrings, and add the README and edit-styles sentences.", "phase": "Design"}
{"id": "DES-45", "description": "Delete the Svelte comment and add the reconnect bullet to web.md.", "phase": "Design"}
{"id": "ENG-78", "description": "Add the comment gate with its scanners, walk and tests in internal/commentgate.", "phase": "Engineering"}
{"id": "DX-47", "description": "Replace the four plan rules and review rows that name the old gate design, so the plan agrees with this workstream.", "phase": "DX"}
{"id": "ENG-79", "description": "Run the live sign-in check for the corrected anthropic and antigravity constants.", "phase": "Engineering"}
{"id": "ENG-80", "description": "Check the donor commit f2cb47b8 for the Devin constant sources, and run just hashline-model on a host with uv.", "phase": "Engineering"}
```

