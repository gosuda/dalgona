### Verdicts on the sixteen anchors

The owner delegated five design choices with the words "judge with spine" and "with spine taste". Each table gives one row for each anchor: the test it applies, the option it picks, and the fact that decides it. A verdict rests on a measured fact or a line of code, and the paragraph after each table says what the plan does.

#### How the terminal prints finished rows above the dock

Options: (A) no guard: print when the row is ready. (B) Bound only: one print holds at most window rows minus dock rows. (C) Settle: B plus a wait until the frame geometry has been unchanged for 3 frames. (D) Settle and freeze: C plus a frozen view while a print is in flight. (E) Fork the renderer to flush before each print. (F) Leave `tea.Println` and write an own scroll-region renderer.

Pick of the table: C: settle. Bound each print to window rows minus dock rows, and wait until the frame geometry has been unchanged for 3 frames (50 ms) before printing.

Rejected: A (no guard): leaves dock lines in history in 9 of 10 same-update dock shrinks and loses all rows in 10 of 10 alternate-screen exits. B (bound only): fails 95 of 100 soak seeds, so bounding by height does not order the print with the flush. D (settle and freeze): the 50 ms soak alone passed with or without the frozen view, which led to rejecting D. The grow case below reverses that, and the plan takes D. D changes `View()` (internal/tui/model.go:637-640). E (fork the renderer to flush before each print): fixes the ordering at its source, but the fork is untested here and adds a maintained dependency; it is the fallback if the wait fails on a slower host. F (own scroll-region renderer): replaces the inline path that docs/spec/tui.md:23 relies on, with no measured need.

| Anchor | Test applied | Pick | Deciding fact |
|---|---|---|---|
| Alexander | Does the change make the whole print path more coherent, or fix one print and break the rest? | C | A leaves dock lines in history in 9 of 10 runs and loses rows in 10 of 10 alternate-screen exits; C had 0 failures in 100 seeds. |
| Feynman | Does the claim hold against the runs that cut against it? | C | The 0 and 5 ms waits fail and 17, 34 and 50 ms pass; C names the loaded tail (40.5 ms) that sets the 50 ms value and reports the 34 ms pass; no freeze claim is made without a measured effect. |
| Shannon | What must the print channel carry, and does the wait keep it inside its limit? | C | The flush delay after a view change has p99 17.4 ms and a loaded maximum of 40.5 ms; a 50 ms settle clears that tail, while bounding by height alone fails 95 of 100 seeds. |
| Hoare | Is the precondition for a print named and checkable? | C | C names the precondition (frame geometry unchanged for 3 frames), and the synctest test passes with the wait and fails without it. |
| Rams | Does each element earn its place, and does the cut keep the function? | C | A keeps rows only by dropping the wait, which loses rows; C adds one counter and one wait; D's freeze is needed for the grow case (print rule 3), which the 50 ms soak did not test. |
| Parnas | Is the likely-to-change flush timing hidden behind one interface? | C | Five production `tea.Println` callers exist today (internal/tui/model.go:382, :449; internal/tui/commands.go:573, :575, :588); C routes them through one drain, so an upstream fix removes one wait; E hides the decision inside a library fork. |
| Tufte | Does the design keep the inconvenient evidence visible? | C | A removes the failing rows by never waiting; C keeps the whole record, with 0 and 5 ms failing and 17 to 50 ms passing. |
| Carmack | Does the abstraction match the measured machine? | C | The wait and the freeze are the options with a measured result on this setup (tmux 3.6, Bubble Tea v2.1.1 pinned at go.mod:14), and the 50 ms bound exceeds the measured loaded maximum of 40.5 ms. |
| Calvino | Is the rule exact and light? | C | One constant and one settle condition say exactly when a print starts; D adds a freeze state, E a fork, and F a renderer. |
| Simon | Does the option stop where the required quality is met? | C | The required quality is 0 failures in the soak; C reaches the soak target, and D's freeze is needed for the grow case, which C does not pass. |
| Hara | Does the user keep the live dock and the way in? | C | C leaves the dock live and the print rule unchanged; D withholds the live dock while a print is in flight. |
| Tolkien | Does the name match the mechanism that was measured? | C | The measured mechanism is the flush delay after a view change, which 'settle' names; 'freeze' names a view hold that the grow case shows is needed. |
| Dijkstra | Can correctness be argued by ordering, not by a timing sample? | E | The race is an ordering fact: insertAbove reads cellbuf.Height() (cursed_renderer.go:781) and flush resizes that buffer (cursed_renderer.go:296-368); flush before print makes the order a construction, while C rests on a suite. |
| Erickson | Does the route change without changing the facts? | C | C adapts timing and keeps every row printed once, in order, as docs/spec/tui.md:23 requires; the facts stay the same. |
| Bass | Is there one accent on the print path and flat ground elsewhere? | C | One wait in one place; D adds a second accent on the view. |
| Aicher | Does one print rule govern every print site? | C | C gives all five print sites one rule and keeps the verb the spec names; F invents a second renderer convention beside Bubble Tea. |

Counts: A 0, B 0, C 15, D 0, E 1, F 0.

Strongest argument against the pick: Strongest argument against C: 50 ms is a timing guess measured on one tmux 3.6 host, and a slower or busier host can exceed the 40.5 ms loaded maximum; each print also gains up to 50 ms of latency that the facts do not price. Dijkstra's case for E: a passing suite stands in for an argument it cannot be, while the ordering fact is in the renderer (cursed_renderer.go:781 reads cellbuf.Height(), and flush at cursed_renderer.go:296-368 is the writer), so flush-before-print makes correctness a construction. Overturned by: a soak on a slower or loaded host that fails with the 50 ms settle; a per-print latency the owner rejects; or a fork that passes the same soak with no wait, which would move the pick to E.

The plan takes option D, not the table's pick C. A later measurement was not in front of the judge: with the real program in `testing/synctest` against the `x/vt` emulator, a message that grew the dock from 3 to 12 rows while a print of 20 rows was in flight left dock lines in the history in 60 of 60 runs without the frozen view and in 0 of 60 runs with it. The frozen view is one branch in `View()`, and it makes the bound of a print hold by construction, which is the dissent's point. When the dock shrank while a print that fitted was in flight, 0 of 300 runs failed with or without the frozen view.

#### How the edit review handles an edit too large for one request

Options: (A) cut the state at a fixed size. (B) One request for each file. (C) Pack whole hunks in order into parts of at most 30,000 encoded bytes, split an oversized hunk at a line boundary, at most 8 parts. (D) Parse each language to split at declarations. (E) Send the long request and accept the long-context mode.

Pick of the table: C: pack whole hunks in order into parts of at most 30,000 encoded bytes; split an oversized hunk at a blank line; at most 8 parts; the worst part's verdict wins.

Rejected: A (cut the state at a fixed size, old plan 48 KiB): 48 KiB is 49,152 bytes, above the roughly 32,000 byte boundary, so it still enters the long-context mode, and the cut silently drops content. B (one request per file): repeats the 7,364-byte question set in every request, bounds nothing for a large file, and cannot answer `incomplete`, which needs the whole turn. D (parse each language to split at declarations): needs a parser and a docs/decisions/dependencies.md row per language; a diff hunk is a fragment and not a whole file, and no fact shows an accuracy gain. E (long request, accept long-context mode): the only option that keeps the whole turn in one request, but the guide says that mode is slower and less accurate, and one unit has an 8 s limit.

| Anchor | Test applied | Pick | Deciding fact |
|---|---|---|---|
| Alexander | Does splitting keep the whole review coherent? | C | Hunks stay whole and in order; only `incomplete` loses the whole turn on large turns, while E puts all 14 questions into the less accurate mode. |
| Feynman | Does the design state its own losses? | C | C names its two losses (worst part wins; `incomplete` on split turns); E's losses (slower, less accurate) are documented, and C's are narrower. |
| Shannon | Is each request kept inside the channel's accurate range? | C | The guide puts requests above about 32,000 bytes in the less accurate mode; C caps each part at 30,000 encoded bytes, of which the 14 questions take 7,364. |
| Hoare | Is each question's input precondition named and met? | E | E is the only option where `incomplete` always sees the whole turn; the accuracy loss weakens the result but does not break a stated precondition. |
| Rams | Does each option keep needed detail and add only what the use needs? | C | A removes needed detail; D adds a parser per language the facts do not require; C removes nothing a question reads, and the 44% context lines stay with their hunks. |
| Parnas | Is the size threshold hidden behind one interface? | C | One constant (30,000 bytes) and one packing function hold the threshold; B and D spread file or language rules into every caller. |
| Tufte | Is the evidence kept readable? | C | A cuts at 49,152 bytes, past the boundary and silently; E keeps the evidence but reads it less accurately; C keeps every hunk within the standard mode. |
| Carmack | Does the design match the measured limits? | C | The roughly 32,000 byte mode boundary and the 8 s unit limit are measured facts; C stays under the first and E risks the second; the price of 0.042 USD per million input tokens is not the constraint. |
| Calvino | Is the rule exact and light? | C | Three exact rules: pack whole hunks in order, split at a blank line, cap at 8 parts; D needs a parser per language. |
| Simon | Does the option stop at the required quality? | C | The required quality is accuracy in the standard mode; C reaches it, and D's declaration-level split would improve boundaries beyond any measured need. |
| Hara | Does the judge get what it needs without being told the answer? | C | Each part carries whole hunks, context and the question set, and the judge decides; A withholds content and B withholds the rest of the turn. |
| Tolkien | Do the names match the diff's own structure? | C | 'Hunk' and 'part' name units a diff already has; A's 'cut' names a loss. |
| Dijkstra | Is the construction small enough for one head to check? | C | The packing rule is one ordered pass with two cases (fits, or split at a blank line); E is one call whose accuracy no reasoning establishes; D needs a parser per language. |
| Erickson | Does the route change without changing the facts? | C | C changes how the judge is fed, not what it is asked; A changes what it sees; B changes what it asks about the turn. |
| Bass | One accent or many? | C | One packing rule with one cap; D adds a second mechanism per language. |
| Aicher | One grammar across all units? | C | One rule covers file units and turn units; D needs a different grammar per language, and B a different shape per file. |

Counts: A 0, B 0, C 15, D 0, E 1.

Strongest argument against the pick: Strongest argument against C: `incomplete` is judged on part of a large turn, and worst-part-wins turns one part's false flag into the verdict; no fact measures that false-flag rate. The 8-part cap (240,000 bytes) also drops hunks beyond it, so the cap must report what it omitted. Overturned by: a labelled set showing `incomplete` false flags on split turns above an acceptable rate (then E, which Hoare favours); a measurement that long-context accuracy equals standard accuracy (then E is cheaper to build); or real turns above 240,000 bytes (then C's cap drops evidence).

The plan takes the table's pick. The dissent is met: the turn unit stays one request, so `incomplete` sees the whole turn.

#### The engine behind the indexed grep

Options: (A) codesearch as a candidate filter inside the search child, with a manifest that trusts a file only when size, modification time and change time match. (B) zoekt as the engine for matches. (C) zoekt as a candidate filter. (D) codesearch as the engine for matches.

Pick of the table: A: codesearch as a candidate filter inside the search child process. The manifest trusts an indexed file only while its size, mtime and ctime match. Files codesearch does not index always stay candidates, and the existing Go regexp matcher decides every match.

Rejected: B zoekt as match engine: it replaces the matcher and the output, cuts the content of files over 2 MiB by default (zoekt index/builder.go SetDefaults, the zoekt source), and adds 58 direct requires. C zoekt as candidate filter: the same 58 requires and the same 2 MiB cut, a second query grammar inside the tool, and an index package its own author says is not public API (the zoekt source). D codesearch as match engine: its Grep writes text through an io.Writer and exposes no line-number value a caller can read, so search output cannot stay byte-identical (the codesearch v1.2.0 source, regexp/match.go).

| Anchor | Test applied | Pick | Deciding fact |
|---|---|---|---|
| Alexander | Does the change repair the whole without adding a parallel rule set? | A | One matcher and one output path stay (engine.go:287 scanFile). zoekt would add its own size cut, binary rule and query grammar. |
| Feynman | Can the author rebuild each claim from source? | A | Each codesearch API name in the codesearch v1.2.0 source is marked Verified against v1.2.0 source. zoekt's index package doc says it is not public API (the zoekt source). |
| Shannon | Does the filter keep every message the search could return? | A | codesearch skips long-line and high-trigram text files silently (write.go constants, the codesearch v1.2.0 source), so A keeps them as candidates. zoekt drops content above 2 MiB (SetDefaults), while engine.go:20 searches up to 16 MiB. |
| Hoare | Is each precondition named and checked by the API? | B | zoekt NewBuilder returns an error for an empty Name (index/builder.go, the zoekt source). codesearch Open returns no error and calls log.Fatal on a corrupt index (index/read.go, the codesearch v1.2.0 source). |
| Rams | Does each element earn its place? | A | codesearch has no module requirements. zoekt has 58 direct requires, including grpc and prometheus (the zoekt source). |
| Parnas | Is the volatile engine choice hidden behind one boundary? | A | A confines the engine to one candidate function that returns file IDs. zoekt's Query and FileMatch types would reach the search rules (the zoekt source, query section). |
| Tufte | Does the change remove evidence the reader needs? | A | A removes no file the matcher would search. zoekt's default 2 MiB cut replaces content with a marker, so matches in larger files disappear (builder.go SetDefaults and Add, the zoekt source). |
| Carmack | Is the cost measured on the path that runs? | A | BenchmarkGrepManyFiles (par_test.go:46) already measures the current matcher. A keeps that matcher, so the same harness measures the filter. No option has a recorded number. |
| Calvino | Is the change exact and light? | A | One file-level filter with no module requirements, against zoekt's 58 direct requires and a non-public index format. |
| Simon | Does the option meet the stated bar and stop there? | A | The bar is identical output and no missed match. zoekt's mmap file builds only on linux, darwin, bsd and illumos (indexfile.go build line, the zoekt source), and the repo already carries Windows files (internal/auth/home_windows.go:1), so zoekt falls short. |
| Hara | Does the tool keep the user's choices? | A | zoekt sets case from the pattern in auto mode (query/query.go setCase, the zoekt source), which overrides the explicit ignore_case flag at search.go:29. A keeps the flag's meaning. |
| Tolkien | Does the name match what the tool does? | A | The tool is named search, and its grep mode means an RE2 regex per line (search.go:28). Under zoekt the word grep would carry zoekt's query semantics. |
| Dijkstra | Can the correctness argument be checked by one reader? | A | The argument is that the candidate set contains every match and the unchanged matcher decides. Merge has open defects (the codesearch v1.2.0 source, issues 50 and 55), so A rebuilds each root and does not call Merge. |
| Erickson | Does the change start from the reader's frame? | A | The model reads the same lines in the same format (engine.go renderGrep, search.go:28). |
| Bass | Is there one accent with flat support beneath it? | A | The matcher stays the one accent, and the index sits flat under it. zoekt would add ranking and ctags as a second accent (the zoekt source). |
| Aicher | Is there one grammar across the family of surfaces? | A | find and grep share one args struct and one parser (json.go:15, json.go:87). zoekt would add its own query grammar with case auto. |

Picks: A 15, B 1 (Hoare), C 0, D 0.

Strongest argument against the pick: The strongest case against A is that codesearch is stale: last tag 2020, 33 open issues, open Merge defects, and log.Fatal on a corrupt index. A failure would surface as a failed search call rather than a fallback to the current scan, because the child boundary turns a non-zero exit into an error (search.go:86). Overturn evidence: a run of BenchmarkGrepManyFiles on the dalgona tree (par_test.go:46) where the filter does not beat the current walk, or a corpus where codesearch skips a large share of the text files the matcher reads.

The plan takes the table's pick.

#### Where symbol lookup and syntax queries appear to the model

Options: (A) new modes of `search`. (B) Three new tools, one for each mode. (C) One new tool `code` with modes. (D) An outline mode of `read` plus a `symbols` mode of `search`.

Pick of the table: A: three new modes of the search tool, symbols, definition and syntax. A file path as scope gives the outline of one file.

Rejected: B three new tools: under eval-first each is folded out of the model's list (mode.go:41-54, FoldedTools returns every tool not in the visible list), so the model cannot call it directly unless evalFirstNames changes (mode.go:13). Each adds a name. C one tool code with modes: the same folding and one new name, and it repeats the args and footer that search already owns. D outline mode on read plus symbols mode on search: read's schema has no mode field (read.go:46), and no part of D gives syntax queries a home.

| Anchor | Test applied | Pick | Deciding fact |
|---|---|---|---|
| Alexander | Does the change join existing parts rather than add a parallel path? | A | search already owns the child process, ignore rules, footers and args (search.go:49-50, json.go:15). A new tool would repeat them. |
| Feynman | Can each claim be explained by its mechanism with its limits stated? | B | Each tool can state its own mechanism and limit. Tags cover 84 of 206 languages (the gotreesitter outline document) and no cross-file relation is inferred (the gotreesitter API guide). A single search description would blur five mechanisms. |
| Shannon | Does the tool list carry each needed message without spending names? | A | A adds no name to the model's list. The visible list has six names (mode.go:13). D cannot carry a syntax query at all. |
| Hoare | Is each mode's precondition explicit in the contract? | B | search has one schema shared by all modes (search.go:29), and find ignores literal and ignore_case (engine.go:187-188 uses only Pattern and Limit). Three tools would each state its own required fields. |
| Rams | Does each new element earn its place? | A | A adds three modes and zero names. B adds three names that are folded under eval-first (mode.go:41-54). |
| Parnas | Is the volatile parser decision hidden behind one boundary? | A | All parser code stays in internal/tool/search, and the model sees only a mode. gotreesitter is pre-v1 and ships breaking changes in v0.x minors (the gotreesitter documents). |
| Tufte | Does each description carry only its own evidence? | B | The search description is already one long paragraph (search.go:28). Three more mechanisms in it add text without data. |
| Carmack | Is the new cost measured on a path that runs? | A | The symbol parse would run in the same child and be measured by the same harness as BenchmarkGrepManyFiles (par_test.go:46). The 8.57 ms gotreesitter figure is Go-only on another machine (BENCH.md), so no speed claim exists for this repo. |
| Calvino | Is the change exact and light? | A | Three modes sharing one argument set is the smallest exact change. No new tool name. The one schema gains the arguments `kind` and `lang`. |
| Simon | Does the option meet the bar and stop there? | A | The bar is symbols, definition and syntax for the 23 grammars (the gotreesitter documents). gotreesitter covers all 23, so three modes meet it with no new name. |
| Hara | Does the tool leave the choice of question to the reader? | A | One entry point lets the model pick the question, as the find and grep enum already does (search.go:29). Three tools would name the question for it. |
| Tolkien | Does the name match what the tool does? | A | The tool is named search. symbols, definition and syntax are the same act, locating things in the workspace, and use the mode vocabulary find and grep already use (search.go:28). |
| Dijkstra | Does each path stay single purpose and short to reason about? | B | execute sends every non-find mode to grep (engine.go:48-51). A adds three modes to that fall-through unless dispatch is made explicit. B gives each tool one path. |
| Erickson | Does the change start from what the model already does? | A | In eval-first the model already calls search for find and grep (mode.go:13). New modes extend a tool it already uses. |
| Bass | Is there one accent with flat ground? | A | search stays the one locate tool. Three new names would become three accents. |
| Aicher | Is there one grammar across the family of surfaces? | A | find and grep share one args struct and one parser (json.go:15, json.go:87). read's schema is a different grammar (read.go:46), so D breaks the family. |

Picks: A 12, B 4 (Feynman, Hoare, Tufte, Dijkstra), C 0, D 0.

Strongest argument against the pick: The strongest case against A is that one pattern argument would mean three things (line regex, symbol name, tree-sitter query), and execute sends any non-find mode to grep (engine.go:48-51), so a new mode can fail silently into grep. Dispatch must then be made explicit, with an error for an unknown mode. Overturn evidence: a measured rate of wrong-mode calls in model traces, or a measured token cost of the longer search description that exceeds the cost of one extra tool name.

The plan takes the table's pick, and the owner chose the same on 2026-11-12. The owner also asked for a third mode, `definition`.

#### The default allow list of a script cell and the default tool mode

Options: (A) a cell may call every tool except `eval` and `yield` in every mode, and it refuses `ask` and the `approve` action of `ratchet` before the allow check; `eval-first` is the default for both products, and `dal` gains the eval tool and the tool modes only. (B) The same, but `dal` keeps no `eval` and stays `normal`. (C) Keep the allow lists for each mode and change only the default. (D) The whole script engine moves into `dal`.

Pick of the table: B: every tool except `eval`, `yield`, `ask` and the `approve` action of `ratchet` is callable from a cell in normal, eval-first and eval-only modes. eval-first becomes the default for dalgona. dal keeps no eval tool and stays in normal mode.

Rejected: A needs internal/script in cmd/dal, which the dal depguard rule denies (.golangci.yml:171-172), and two-products.md:5 says dal must stay the minimal agent. C keeps the eval-first cell list at the folded tools (mode.go:60-61), so read, search, patch and exec stay out of reach from cells, which misses the owner's request. D moves the script engine into dal, which the same depguard denial and the same two-products invariant forbid.

| Anchor | Test applied | Pick | Deciding fact |
|---|---|---|---|
| Alexander | Does the change keep the product whole? | B | eval-only already applies every tool except eval (mode.go:62-63, eval.go:43). One allow rule then covers both cell modes, and dal stays whole. |
| Feynman | Can the author state why it holds, including its limit? | B | The approval gate decides exec, not the list. mechanical accepts read, agent and script calls and not exec (gate.go:125-134). Nested calls go through the same admission (extension.go:87). |
| Shannon | Does the restriction carry information or only remove capacity? | B | eval-first removes read, search, patch and exec from cells (mode.go:60-61), and the gate still judges every exec call. The restriction removes capacity the gate already governs. |
| Hoare | Is each rule an explicit, checked contract? | B | prepareTool checks the allow list explicitly (ctx.go:119-122). dal's refusal of eval is a stated rule (docs/spec/config.md:7, merge.go dalgonaOnlyKeys). A would put script into dal, which the depguard forbids (.golangci.yml:171-172). |
| Rams | Does each element earn its place? | B | B changes DefaultUses (mode.go:55-72) and one default. A adds an eval tool to dal, which two-products.md:5 forbids. |
| Parnas | Is the decision hidden behind one boundary? | B | DefaultUses alone decides a cell's allow list. dal gets no new import edge, while A and D would cross the dal boundary (.golangci.yml:171-172). |
| Tufte | Does the model-visible list show only what it can use? | B | dal's visible list stays as it is. A would add eval to dal, where config already rejects eval (docs/spec/config.md:7). |
| Carmack | Is the new path one that runs in production? | B | eval-only already runs the all-tools allow list through the battery wiring (battery.go:70-71). B adds no path that has not run. |
| Calvino | Is the change light and exact? | B | One predicate and one default. A moves or re-classifies the script package. D moves the whole engine. |
| Simon | Does it meet the owner's bar and stop there? | B | The owner asked for eval-first and callable visible tools. B meets that for dalgona, the only product with cells, and makes no further change. |
| Hara | Does the rule leave the user room to proceed? | B | C blocks exec from cells even when the gate would approve it (mode.go:60-61). B lets the cell proceed and leaves each call to the gate. |
| Tolkien | Does the name match the content? | B | dal has no eval tool and stays normal, so eval-first never names a product without eval. A would give dal eval-first while the lint calls its script package a battery. |
| Dijkstra | Can the reasoning be checked in one head? | B | A cell reaches a tool through two checks, reach (ctx.go:79) and allow (ctx.go:120), plus the gate. B changes only the allow default. |
| Erickson | Does the change start from the model's existing frame? | B | In eval-first the model already sees read, search, patch and exec (mode.go:13). Cells use the tools it already knows. |
| Bass | Is there one accent with flat ground? | B | eval cells are the one accent in dalgona. dal stays flat with no script engine. |
| Aicher | Is there one rule across the family? | B | One allow rule (every tool except eval) serves both cell modes. dal, with no cells, keeps the product rule that config rejects eval (docs/spec/config.md:7). |

Picks: B 16, A 0, C 0, D 0.

Strongest argument against the pick: The owner asked for eval-first as the default for both products, and B gives dal a normal default that has no effect. config.go:259 is product-blind today, so B adds a product branch. Overturn evidence: the owner accepting a lint and decision change so dal can import the script package, which would make A the pick; or evidence that a cell's exec call escapes the approval gate, which would overturn the gate argument. The change also flips eval_test.go:154-170 and the default test at config_test.go:48-56, and it changes docs/spec/config.md:9 and docs/spec/starlark.md:129-133.

The owner chose option A on 2026-11-12, against the table's pick B: `dal` gains the `eval` tool and the tool modes, and `docs/decisions/two-products.md` changes in Eval default step 2. The table's concern is recorded once here: `dal` grows by the script engine, and its decision record no longer keeps the engine out. The same owner choice makes eval-first the default in dal, so the default at config.go:259 (ToolMode: ToolModeNormal) changes for both products, and tool_mode and eval leave the dalgonaOnlyKeys map in merge.go.

