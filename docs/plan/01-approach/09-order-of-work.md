### Order of work

The work has 21 phases. A phase is done when its last step is committed and `just verify` passes.

1. Make the load-sensitive tests deterministic.
2. Raise the stale version pins.
3. Remove the wrappers that hide nothing.
4. Fix the verified core defects.
5. Correct the documents and rewrite the specifications.
6. Add the diff package.
7. Complete the wire protocol.
8. Carry image input to the model.
9. Add the ask tool.
10. Add the opt-in edit review.
11. Add the ratchet tool.
12. Write the design record, the product marks and `AGENTS.md`.
13. Build the live-debug tooling.
14. Rebuild the terminal interface.
15. Rebuild the web interface.
16. Make `eval-first` the default and open the tools to script cells.
17. Index the `grep` mode of the search tool.
18. Add the `symbols`, `definition` and `syntax` modes.
19. Add the structural rewrite tool.
20. Remove every prose comment from Go, Svelte, Python and hand-written TypeScript files, including doc comments on exported names and the module docstring of `docs/spec/hashline/verify_model.py`. Keep compiler directives and `//nolint:<linter> // reason` lines.
21. Run the live sessions.

The table gives the one order of all steps. Do the steps from the top. A step's text is under its workstream, at the heading `<Workstream> step <id>.` The column `Lands after` names the steps that a step needs; the order already obeys it. A workstream can span phases: for example, the terminal card of the ask tool lands in phase 14.

| No. | Phase | Step | Lands after |
|---|---|---|---|
| 1 | 1 | Tests-and-docs step 1. In-memory model transport for the agent harness |  |
| 2 | 1 | Tests-and-docs step 2. `internal/agent/request_models_test.go:173` |  |
| 3 | 1 | Tests-and-docs step 3. `internal/agent/childcap_test.go:76` |  |
| 4 | 1 | Tests-and-docs step 4. `internal/agent/misc_test.go:309` |  |
| 5 | 1 | Tests-and-docs step 5. `internal/agent/steer_atomic_test.go:82` |  |
| 6 | 1 | Tests-and-docs step 6. `internal/agent/override_test.go:190` |  |
| 7 | 1 | Tests-and-docs step 7. `internal/runner/local_test.go:410` and `:301` |  |
| 8 | 1 | Tests-and-docs step 8. `internal/runner/local_test.go:230, 253, 267, 430`, shim gate |  |
| 9 | 1 | Tests-and-docs step 9. `internal/runner/process_group_linux_test.go:55` |  |
| 10 | 1 | Tests-and-docs step 10. `internal/runner/remote_atomic_write_test.go:41` |  |
| 11 | 1 | Tests-and-docs step 11. `internal/daemon/run_test.go:66` |  |
| 12 | 2 | Pins step 1. Raise the Go toolchain to go1.27.4 |  |
| 13 | 2 | Pins step 2. Raise golang.org/x/net and golang.org/x/text (advisory group) |  |
| 14 | 2 | Pins step 3. Raise lipgloss, go-landlock, x/sync, and x/sys; tidy settles ultraviolet |  |
| 15 | 2 | Pins step 4. Raise @sveltejs/kit to 3.2.0 and vite to 8.3.8 |  |
| 16 | 2 | Pins step 5. Rewrite the grounded pins in both decision files |  |
| 17 | 3 | Wrappers step 1. Delete 14 forwarding functions and move their callers |  |
| 18 | 4 | Core-fixes step 1. Retry-After over 60 s stops the request (product decision 13) |  |
| 19 | 4 | Core-fixes step 2. Quota 429 and 402 are terminal (product decision 15) |  |
| 20 | 4 | Core-fixes step 3. Anthropic passes `xhigh` and `max` through (product decision 14) |  |
| 21 | 4 | Core-fixes step 4. Keep and replay Anthropic `redacted_thinking` (product decision 15) |  |
| 22 | 4 | Core-fixes step 5. Compaction must shrink (product decision 15) |  |
| 23 | 4 | Core-fixes step 6. Provider-safe MCP tool names (product decision 15) |  |
| 24 | 4 | Core-fixes step 7. Refuse omission placeholders in edits (product decision 15) |  |
| 25 | 4 | Core-fixes step 8. One exit wait that honours the caller's context |  |
| 26 | 4 | Core-fixes step 9. One shared Connect framing package |  |
| 27 | 4 | Core-fixes step 10. Pinned writes on Windows (product decision 2) |  |
| 28 | 5 | Tests-and-docs step 12. Hashline symbol, status and link drift |  |
| 29 | 5 | Tests-and-docs step 13. Delete `approval_stale` from the spec | Tests-and-docs 12 |
| 30 | 5 | Tests-and-docs step 15. Terms list |  |
| 31 | 5 | Tests-and-docs step 16. Decision rules and rejected alternatives | Tests-and-docs 12 |
| 32 | 5 | Tests-and-docs step 17. Process-group escape limit |  |
| 33 | 5 | Tests-and-docs step 18. Mutation-probe rule |  |
| 34 | 5 | Spec-fix step 1. GrantPrefix rejects an unterminated quote or a trailing backslash |  |
| 35 | 5 | Spec-fix step 2. Resolve every writable root with its longest existing prefix |  |
| 36 | 5 | Spec-fix step 3. Reject an empty `mcp_trusted_workspaces` entry at load |  |
| 37 | 5 | Spec-fix step 4. Always send `model`, and fail an empty model before the request |  |
| 38 | 5 | Spec-fix step 5. Ignore instruction files that resolve outside their root |  |
| 39 | 5 | Spec-fix step 6. Resolve symbolic links in manifest paths before the containment check |  |
| 40 | 5 | Spec-fix step 7. Reject NUL bytes anywhere in the file |  |
| 41 | 5 | Spec-fix step 8. Reject volume-named and rooted paths on every OS |  |
| 42 | 5 | Spec-fix step 9. Report a changed script effect replay as a mismatch |  |
| 43 | 5 | Spec-fix step 10. Count the websocket box token in characters, not bytes |  |
| 44 | 5 | Spec-fix step 11. Stop the verifier from rewriting the report it is checked against |  |
| 45 | 5 | Spec-fix step 12. Light tool description states the REM display exception |  |
| 46 | 5 | Spec-fix step 13. Modal Destroy deletes the volume when the sandbox is already gone |  |
| 47 | 5 | Spec-fix step 14. Report off for yolo in EffectiveSandbox |  |
| 48 | 5 | Spec-fix step 15. A completed script run returns its recorded outcome |  |
| 49 | 5 | Spec-fix step 16. Test the Cloudflare refusal at the command level |  |
| 50 | 5 | Specifications step 1. Add the style check for `docs/spec` |  |
| 51 | 5 | Specifications step 2. Correct `docs/spec/advisor.md` |  |
| 52 | 5 | Specifications step 3. Correct `docs/spec/approval-rules.md` |  |
| 53 | 5 | Specifications step 4. Correct `docs/spec/approvals.md` |  |
| 54 | 5 | Specifications step 5. Correct `docs/spec/codex-app.md` |  |
| 55 | 5 | Specifications step 6. Correct `docs/spec/config.md` |  |
| 56 | 5 | Specifications step 7. Correct `docs/spec/daemon.md` |  |
| 57 | 5 | Specifications step 8. Correct `docs/spec/decide.md` |  |
| 58 | 5 | Specifications step 9. Correct `docs/spec/discovery.md` |  |
| 59 | 5 | Specifications step 10. Correct `docs/spec/edit-styles.md` |  |
| 60 | 5 | Specifications step 11. Correct `docs/spec/events.md` |  |
| 61 | 5 | Specifications step 12. Correct `docs/spec/exec-server.md` |  |
| 62 | 5 | Specifications step 13. Correct `docs/spec/hashline/README.md` |  |
| 63 | 5 | Specifications step 14. Correct `docs/spec/hashline/hashline-common-v1.md` |  |
| 64 | 5 | Specifications step 15. Correct `docs/spec/hashline/hashline-conformance-v1.md` |  |
| 65 | 5 | Specifications step 16. Correct `docs/spec/hashline/hashline-enhanced-v1.md` |  |
| 66 | 5 | Specifications step 17. Correct `docs/spec/hashline/hashline-light-v1.md` |  |
| 67 | 5 | Specifications step 18. Correct `docs/spec/hashline/hashline-review.md` |  |
| 68 | 5 | Specifications step 19. Correct `docs/spec/herdr.md` |  |
| 69 | 5 | Specifications step 20. Correct `docs/spec/journal.md` |  |
| 70 | 5 | Specifications step 21. Correct `docs/spec/keywords.md` |  |
| 71 | 5 | Specifications step 22. Correct `docs/spec/mcp.md` |  |
| 72 | 5 | Specifications step 23. Correct `docs/spec/oauth.md` |  |
| 73 | 5 | Specifications step 24. Correct `docs/spec/protocol.md` |  |
| 74 | 5 | Specifications step 25. Correct `docs/spec/providers.md` |  |
| 75 | 5 | Specifications step 26. Correct `docs/spec/recovery.md` |  |
| 76 | 5 | Specifications step 27. Correct `docs/spec/runner.md` |  |
| 77 | 5 | Specifications step 28. Correct `docs/spec/runners.md` |  |
| 78 | 5 | Specifications step 29. Correct `docs/spec/sandbox-services.md` |  |
| 79 | 5 | Specifications step 30. Correct `docs/spec/sandbox.md` |  |
| 80 | 5 | Specifications step 31. Correct `docs/spec/search.md` |  |
| 81 | 5 | Specifications step 32. Correct `docs/spec/secrets.md` |  |
| 82 | 5 | Specifications step 33. Correct `docs/spec/starlark.md` |  |
| 83 | 5 | Specifications step 34. Correct `docs/spec/tui.md` |  |
| 84 | 5 | Specifications step 35. Correct `docs/spec/web.md` |  |
| 85 | 5 | Specifications step 36. Correct `docs/decisions/minimal-harness-sandbox.md` |  |
| 86 | 5 | Specifications step 37. Correct `docs/decisions/model-presets.md` |  |
| 87 | 5 | Specifications step 38. Correct `docs/decisions/secret-masking.md` |  |
| 88 | 5 | Specifications step 39. Correct `docs/decisions/two-products.md` |  |
| 89 | 5 | Specifications step 39a. Correct `docs/decisions/codex-protocol-as-core.md` |  |
| 90 | 5 | Specifications step 40. Make the style check a gate | Tests-and-docs 12, 13, 15 to 18; Specifications 1 to 39a |
| 91 | 6 | Edit-review step 1. Line diff package |  |
| 92 | 7 | Edit-review step 3. Config table `[edit_review]` |  |
| 93 | 7 | Protocol step 1. Dalgona schemas and client guarantees |  |
| 94 | 7 | Core-fixes step 1a. Attended waits show a countdown (product decision 13) | Core-fixes 1; Protocol 1 |
| 95 | 7 | Protocol step 2. Journal times, turn and command durations |  |
| 96 | 7 | Protocol step 3. Context window and token usage on resume |  |
| 97 | 7 | Protocol step 4. Unified diffs with line counts |  |
| 98 | 7 | Protocol step 6. Spill metadata and output pages |  |
| 99 | 7 | Protocol step 7. Keyword matches from the server |  |
| 100 | 7 | Protocol step 8. Interactive settings keys | Edit-review 3 |
| 101 | 7 | Protocol step 9. The host owns live settings | Protocol 8 |
| 102 | 7 | Protocol step 10. `config/batchWrite` and `dalgona/settings/read` | Protocol 8, 9 |
| 103 | 7 | Protocol step 11. Model list for `/model` |  |
| 104 | 7 | Protocol step 12. `/login` for API-key providers |  |
| 105 | 7 | Core-fixes step 11. Device-code login for headless hosts (product decision 15) | Protocol 12 |
| 106 | 7 | Protocol step 14. `review/start` |  |
| 107 | 7 | Protocol step 15. `mcpServerStatus/list` |  |
| 108 | 7 | Protocol step 16. `command/exec` and `command/exec/terminate` |  |
| 109 | 7 | Protocol step 17. `fs/*` file methods |  |
| 110 | 7 | Protocol step 18. `fs/watch` and `fs/unwatch` |  |
| 111 | 7 | Protocol step 19. `fuzzyFileSearch` |  |
| 112 | 8 | Image-input step 1. Image limits and type detection |  |
| 113 | 8 | Image-input step 3. Journal form of an image attachment | Image-input 1 |
| 114 | 8 | Image-input step 4. Submit stores the image blob and masks the path | Image-input 1, 3 |
| 115 | 8 | Image-input step 2. Input variants on `turn/start` and `turn/steer` | Protocol 9; Image-input 1, 3, 4 |
| 116 | 8 | Image-input step 5. Model content part, transcript and blob integrity | Image-input 1, 3, 4 |
| 117 | 8 | Image-input step 6. `inputModalities` in `model/list` | Protocol 11; Image-input 5 |
| 118 | 8 | Image-input step 7. Refusal for a model without image input | Protocol 3; Image-input 5, 6 |
| 119 | 8 | Image-input step 8. Anthropic sends image blocks | Core-fixes 3, 4; Image-input 5 to 7 |
| 120 | 8 | Image-input step 9. OpenAI Responses and ChatGPT send `input_image` | Image-input 5 to 7 |
| 121 | 8 | Image-input step 10. Google sends `inlineData` | Image-input 5 to 7 |
| 122 | 8 | Image-input step 11. Chat-compatible adapters send `image_url` and read OpenRouter modalities | Image-input 5 to 7 |
| 123 | 8 | Image-input step 12. Devin sends `Images` on the prompt and reads `SupportsImages` | Image-input 5 to 7 |
| 124 | 8 | Image-input step 13. The `userMessage` item echoes image metadata | Protocol 2, 7; Image-input 2 to 4 |
| 125 | 9 | Ask step 1. Add package `internal/tool/ask` |  |
| 126 | 9 | Ask step 2. Agent host path for questions | Ask 1 |
| 127 | 9 | Ask step 3. Server path on `item/tool/requestUserInput` | Ask 2 |
| 128 | 9 | Protocol step 5. Files and diff before an edit approval | Edit-review 1; Protocol 4; Ask 3 |
| 129 | 9 | Ask step 3a. Approval requests follow the question routing rule | Ask 3 |
| 130 | 9 | Ask step 4. Keep `ask` out of Starlark | Ask 1 |
| 131 | 9 | Ask step 5. Register `ask` for top-level threads and recover it | Ask 2, 3 |
| 132 | 9 | Ask step 6. The `-p` client refuses questions | Ask 3 |
| 133 | 10 | Edit-review step 2. Journal event `tool.judged` |  |
| 134 | 10 | Edit-review step 4. The judging hook in the turn loop, journaling, and recovery | Edit-review 1 to 3 |
| 135 | 10 | Edit-review step 5. Feedback block in the model request | Edit-review 2, 4 |
| 136 | 10 | Edit-review step 4a. The turn review | Edit-review 2, 4, 5; Protocol 5 |
| 137 | 10 | Edit-review step 6. Client notification `dalgona/tool/judged` | Edit-review 2, 4, 4a, 5; Protocol 1 |
| 138 | 10 | Edit-review step 9. Precision measurement and ratchet | Edit-review 4, 6 |
| 139 | 11 | Ratchet step 1. Notice loading cannot be empty |  |
| 140 | 11 | Ratchet step 2. Taste notices `decide` and `simplify` | Ratchet 1 |
| 141 | 11 | Ratchet step 3. Statistic, decision rule, plateau and ceiling (`internal/ratchet`, pure) | Edit-review 1 |
| 142 | 11 | Ratchet step 4. Flow state, setup, split, digests and approval status |  |
| 143 | 11 | Ratchet step 5. Results contract, check, gate and train |  |
| 144 | 11 | Ratchet step 6. The `ratchet` tool and the approval seam |  |
| 145 | 11 | Ratchet step 7. Battery wiring, top-level threads only | Ask 1, 2; Ratchet 6 |
| 146 | 11 | Ratchet step 8. The `ratchet` keyword | Ratchet 1, 2 |
| 147 | 12 | Tests-and-docs step 19. `DESIGN.md`, brand marks, icon font and licence notices |  |
| 148 | 12 | Tests-and-docs step 20. `AGENTS.md` |  |
| 149 | 13 | Live-debug step 1. Add `internal/llmstub`, the scripted chat-completions handler |  |
| 150 | 13 | Live-debug step 2. Add the `llmstub` command and the live script | Live-debug 1 |
| 151 | 13 | Live-debug step 3. Add the recipes `omp-tui-install`, `live-up`, `live-down` and `live-check` | Ratchet 3; Live-debug 1, 2 |
| 152 | 14 | Terminal step 1. Split the package by concern |  |
| 153 | 14 | Terminal step 2. One sanitiser for all untrusted text | Terminal 1 |
| 154 | 14 | Terminal step 3. Bound every frame to the window, with frame tests | Terminal 1, 2 |
| 155 | 14 | Terminal step 4. `/revert` asks for confirmation | Terminal 1 to 3 |
| 156 | 14 | Terminal step 5. Follow-ups are never cleared silently | Terminal 1 to 3 |
| 157 | 14 | Terminal step 6. Thread-changing commands are refused during a turn | Terminal 1, 2 |
| 158 | 14 | Terminal step 7. A prompt that starts with `/` can be sent | Terminal 1, 2 |
| 159 | 14 | Terminal step 8. The login notice tells the truth | Protocol 12; Terminal 1, 2 |
| 160 | 14 | Terminal step 9. Themes, no-colour mode and background detection | Tests-and-docs 19; Protocol 8; Terminal 1 to 3 |
| 161 | 14 | Terminal step 10. Keyword mark without motion or colour-only signal | Protocol 7; Terminal 9 |
| 162 | 14 | Terminal step 11. Live row, state words, breathing dot and elapsed counter | Core-fixes 1a; Terminal 1 to 3, 9, 10 |
| 163 | 14 | Terminal step 12. Status band with eight items, whole-item dropping and icons | Protocol 3, 8; Terminal 1, 2, 9, 11 |
| 164 | 14 | Terminal step 13. Hint slot, Esc interrupt guard, Ctrl+C window | Terminal 3, 11 |
| 165 | 14 | Terminal step 14. Commit once, row grammar, tool cards and file diffs | Terminal 2, 3, 9, 11 |
| 166 | 14 | Terminal step 15. Review overlay on `ctrl+o` | Terminal 3, 11, 13, 14 |
| 167 | 14 | Terminal step 16. Verbosity levels and reasoning collapse | Terminal 3, 9, 11, 14, 15 |
| 168 | 14 | Edit-review step 7. Terminal row | Edit-review 6; Terminal 9, 14, 16 |
| 169 | 14 | Terminal step 17. Streaming Markdown | Terminal 3, 9, 11, 14, 15 |
| 170 | 14 | Terminal step 18. Plan panel and subagent block | Terminal 3, 11, 14 |
| 171 | 14 | Terminal step 19. Multi-line editor with a real cursor | Terminal 1 to 3, 9 |
| 172 | 14 | Terminal step 20. Prompt history | Terminal 3, 11, 13, 19 |
| 173 | 14 | Terminal step 21. List panels and the model picker with presets | Terminal 1 to 3, 11 to 13, 19 |
| 174 | 14 | Terminal step 22. Slash palette | Terminal 7, 19, 21 |
| 175 | 14 | Terminal step 23. Every remaining slash command opens a panel | Terminal 4, 6, 7, 21 |
| 176 | 14 | Terminal step 24. `/settings` panel | Protocol 8, 10; Terminal 3, 7, 9, 12, 16, 21 |
| 177 | 14 | Terminal step 25. `/login` panel | Protocol 12; Terminal 1, 3, 8, 21 |
| 178 | 14 | Tests-and-docs step 14. `README.md` and `/login` | Protocol 12; Terminal 25 |
| 179 | 14 | Terminal step 26. Approval card content | Terminal 1, 3, 15, 21 |
| 180 | 14 | Terminal step 28. Paste chips | Terminal 2, 3, 19, 21 |
| 181 | 14 | Terminal step 29. `@file` completion | Terminal 3, 19, 21, 22 |
| 182 | 14 | Terminal step 30. `!` shell mode | Terminal 3, 13, 14, 16, 19, 21, 28 |
| 183 | 14 | Terminal step 31. Image paste | Image-input 1, 2, 6, 7; Terminal 19, 21, 28 |
| 184 | 14 | Terminal step 32. Visible follow-up queue | Terminal 3, 11 to 13, 19, 21 |
| 185 | 14 | Terminal step 33. Rewind gesture | Terminal 3, 4, 11, 13, 21, 30 |
| 186 | 14 | Terminal step 34. Help overlay | Terminal 3, 7, 12, 13, 15, 19 to 22, 30, 32, 33 |
| 187 | 14 | Terminal step 35. Mouse | Terminal 3, 15, 21, 26 |
| 188 | 14 | Terminal step 36. Window title and desktop notifications | Protocol 8; Terminal 2, 3, 9, 11, 12 |
| 189 | 14 | Ask step 7. Terminal question card | Ask 3; Terminal 1, 9, 14, 21, 26, 36 |
| 190 | 14 | Protocol step 13. Sandbox requested but unavailable | Tests-and-docs 16; Ask 2, 3, 3a, 7 |
| 191 | 14 | Terminal step 27. Ask card | Ask 7; Terminal 7, 21, 26, 36 |
| 192 | 14 | Terminal step 37. Welcome and exit lines | Terminal 3, 12, 13, 15 |
| 193 | 15 | Web step 1. Component tests and a composer that keeps focus |  |
| 194 | 15 | Web step 2. Honour the `seq` guard on thread status |  |
| 195 | 15 | Web step 3. Check every parser against the Codex schema | Web 1 |
| 196 | 15 | Web step 4. Move page state into a session | Web 2 |
| 197 | 15 | Web step 8. Palette, tokens and four themes | Web 4 |
| 198 | 15 | Web step 5. Notices in stable live regions | Web 1, 4, 8 |
| 199 | 15 | Web step 6. Name only the recovery controls that are on screen | Web 4, 5 |
| 200 | 15 | Web step 7. Requests waiting on another thread, inside the workspace | Web 4, 5 |
| 201 | 15 | Web step 9. Self-hosted fonts and type roles | Web 4, 8 |
| 202 | 15 | Web step 10. State words, the state tag and duration formats | Web 8, 9 |
| 203 | 15 | Web step 11. Selected thread without colour | Web 8, 10 |
| 204 | 15 | Web step 12. Cap the thread list, not the sidebar |  |
| 205 | 15 | Web step 13. Start thread keeps focus while it works | Web 1, 4 |
| 206 | 15 | Web step 14. One status announcement per turn change | Web 4, 5, 10 |
| 207 | 15 | Web step 15. Markdown and code rendering | Web 1, 8 |
| 208 | 15 | Web step 16. Transcript rows | Web 8, 10, 13 to 15 |
| 209 | 15 | Web step 17. Diff card | Web 16 |
| 210 | 15 | Edit-review step 8. Browser verdict on the file change | Edit-review 6; Web 8, 16, 17 |
| 211 | 15 | Web step 18. Verbosity levels and the review view | Web 4, 16 |
| 212 | 15 | Web step 19. Jump to latest | Web 16 |
| 213 | 15 | Web step 20. Thread header and status band | Web 2, 4, 7, 10 |
| 214 | 15 | Web step 21. Approval card with actions always in view | Web 4, 7, 17 |
| 215 | 15 | Web step 22. Diffs on live file-change approvals | Web 21 |
| 216 | 15 | Web step 23. One request at a time | Web 4, 5, 21 |
| 217 | 15 | Ask step 8. Browser question card in the dock | Ask 3; Web 4, 8, 10, 21, 23 |
| 218 | 15 | Web step 24. Docked composer and 16 px inputs | Web 8, 23 |
| 219 | 15 | Web step 25. Visible follow-up queue | Web 24 |
| 220 | 15 | Web step 27. Composer history | Web 4, 25 |
| 221 | 15 | Web step 26. Esc interrupt guard | Web 25, 27 |
| 222 | 15 | Web step 28. Panels, command palette and the key list | Web 18, 27 |
| 223 | 15 | Web step 30. Settings panel | Web 8, 28 |
| 224 | 15 | Web step 31. Login panel | Web 5, 28 |
| 225 | 15 | Web step 29. Model panel with presets | Web 28, 31 |
| 226 | 15 | Web step 32. Thread command panels and the rewind gesture | Web 16, 18, 28 |
| 227 | 15 | Web step 33. File mention completion | Web 27, 28 |
| 228 | 15 | Web step 35. Paste chips and image paste | Web 25 |
| 229 | 15 | Web step 34. Shell mode | Web 28, 35 |
| 230 | 15 | Web step 36. Notifications | Web 4, 10, 23 |
| 231 | 15 | Web step 37. Layout at 320 px | Web 20, 24, 29 |
| 232 | 15 | Web step 38. Accessibility gates | Web 8, 14, 23 to 37 |
| 233 | 16 | Eval default step 1. Cells reach every tool except eval and yield, refuse calls that wait for a person, and stop at 1024 calls | Ask 4; Ratchet 7 |
| 234 | 16 | Eval default step 2. Give dal the eval tool and the tool modes | Eval default 1; Edit-review 3 |
| 235 | 16 | Eval default step 3. Make eval-first the default for both products | Eval default 2 |
| 236 | 17 | Code search step 1. Baseline benchmarks of the scan path |  |
| 237 | 17 | Code search step 2. Manifest file format | Code search 1 |
| 238 | 17 | Code search step 3. Index builder, publish rules and hidden build command | Code search 2; Tests-and-docs 15; Specifications 31 |
| 239 | 17 | Code search step 4. Trigram query, freshness rule and exit status 1 | Code search 3 |
| 240 | 17 | Code search step 5. Parent uses the index and retries without it | Code search 4 |
| 241 | 17 | Code search step 6. Background build after the uncovered-bytes threshold | Code search 5 |
| 242 | 18 | Code symbols step 1. Syntax package boundary and Go definitions | Edit-review 1; Code search 3 |
| 243 | 18 | Code symbols step 2. Definition queries for TypeScript, TSX, JavaScript, Python and Rust | Code symbols 1 |
| 244 | 18 | Code symbols step 3. Definition queries for Java, C, C++, C# and Kotlin | Code symbols 2 |
| 245 | 18 | Code symbols step 4. Definition queries for Swift, Ruby, PHP, Bash, Lua and Markdown | Code symbols 3 |
| 246 | 18 | Code symbols step 5. Syntax-only grammars for Svelte, HTML, CSS, JSON, YAML and TOML | Code symbols 4 |
| 247 | 18 | Code symbols step 6. The symbols mode of search | Code symbols 5; Specifications 31; Code search 6 |
| 248 | 18 | Code symbols step 7. The definition mode of search | Code symbols 6 |
| 249 | 18 | Code symbols step 8. The syntax mode of search | Code symbols 7 |
| 250 | 18 | Code symbols step 9. Benchmarks and the parse bounds | Code symbols 8 |
| 251 | 19 | Code rewrite step 1. Syntax additions for edits | Code symbols 8 |
| 252 | 19 | Code rewrite step 2. Shared candidate selection | Code symbols 8 |
| 253 | 19 | Code rewrite step 3. Replacement entry in patch | Core-fixes 7 |
| 254 | 19 | Code rewrite step 4. Match and template expansion | Code rewrite 1 |
| 255 | 19 | Code rewrite step 5. The rewrite tool | Code rewrite 2; Code rewrite 3; Code rewrite 4; Eval default 1; Specifications 32; Specifications 40 |
| 256 | 19 | Code rewrite step 6. Rewrite approval, wire and journal tests | Code rewrite 5; Protocol 4; Protocol 5 |
| 257 | 20 | Comment removal step 1. Rule text in CONTRIBUTE.md and REVIEW.md |  |
| 258 | 20 | Comment removal step 2. Remote package comments | Comment removal 1 |
| 259 | 20 | Comment removal step 3. Agent package comments and the CheckRemoteValue rename | Comment removal 2 |
| 260 | 20 | Comment removal step 4. App package comments and the command-line spec | Comment removal 3 |
| 261 | 20 | Comment removal step 5. Client constants from the donor files | Comment removal 4 |
| 262 | 20 | Comment removal step 6. Auth package citations | Comment removal 5 |
| 263 | 20 | Comment removal step 7. Codex app-server comments | Comment removal 6 |
| 264 | 20 | Comment removal step 8. Decide package citation | Comment removal 7 |
| 265 | 20 | Comment removal step 9. MCP package citations | Comment removal 8 |
| 266 | 20 | Comment removal step 10. Tool package comment | Comment removal 9 |
| 267 | 20 | Comment removal step 11. Antigravity citations | Comment removal 10 |
| 268 | 20 | Comment removal step 12. Cursor citations | Comment removal 11 |
| 269 | 20 | Comment removal step 13. Devin citations | Comment removal 12 |
| 270 | 20 | Comment removal step 14. Anthropic model list citation | Comment removal 13 |
| 271 | 20 | Comment removal step 15. Chat-compatible model list citation | Comment removal 14 |
| 272 | 20 | Comment removal step 16. ChatGPT citations | Comment removal 15 |
| 273 | 20 | Comment removal step 17. Model presets citation | Comment removal 16 |
| 274 | 20 | Comment removal step 18. Runner package comments | Comment removal 17 |
| 275 | 20 | Comment removal step 19. Hashline model checker comments | Comment removal 18 |
| 276 | 20 | Comment removal step 20. Web client comment | Comment removal 19 |
| 277 | 20 | Comment removal step 21. Comment gate | Comment removal 20 |
| 278 | 21 | Live-debug step 4. Write the session scripts, the defect loop and the report rules into `docs/spec/live-debug.md` | Live-debug 1 to 3; Terminal 37; Web 38; Eval default 3; Code search 6; Code symbols 9; Code rewrite 6; Comment removal 21 |

