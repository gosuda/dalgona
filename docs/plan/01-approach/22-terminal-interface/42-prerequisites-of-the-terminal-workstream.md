#### Prerequisites of the terminal workstream

- The protocol workstream (must land before the terminal step named):
  - P1 `fuzzyFileSearch`: protocol Step 19, before step 29.
  - P2 `command/exec`: protocol Step 16, before step 30.
  - P3 `diffOmitted` on `item/fileChange/patchUpdated`: protocol Step 4 (unified diffs), before steps 14 and 26. Step 26 falls back to the live item's `Changes` until then.
  - P5 `tokenUsage.modelContextWindow`: protocol Step 3 (context window and token usage), before step 12. The gauge stays hidden at zero.
  - P6 `presets` on `model/list`: protocol Step 11, before step 21.
  - P7 config keys and settings: protocol Step 8 (`Config.Icons`, `StatusHide`, `Verbosity`, `Theme`, `Notifications`), Step 9 (live settings) and Step 10 (`config/batchWrite`, `dalgona/settings/read`), before steps 9, 12, 16, 24 and 36. Steps 9 and 16 read the keys and fall back to `auto` and `medium` when they are absent.
  - P8 image input: the image-input workstream, Steps 1, 2, 6 and 7 (limits, wire form, `inputModalities`, refusal), before step 31.
  - `notifications`: protocol Step 8 (`Config.Notifications`) and Step 10 (`settingKey`), before step 36.
  - `account/login/completed` with `provider`: protocol Step 12, before step 8.
  - L1 `dalgona/account/providers/list`: protocol Step 12, before step 25.
  - Sandbox choice: the protocol workstream, Step 13, with the ask workstream, Steps 2, 3 and 3a, before step 27.
  - `dalgona/retry/wait`: the core-fixes workstream, Step 1a, before step 11.
  - `item/tool/requestUserInput`: the ask workstream, Step 3, before step 27.
- `account/login/start` is read in the current code: `loginStart` and `loginKey` in `internal/rpc/serve_misc.go:206-256` take the `apiKey` and ChatGPT flows, and Step 25 uses them unchanged. L1 (protocol Step 12) is the only login addition.
- The ask workstream, Step 3: the Go types `rpc.ToolRequestUserInputParams` and `rpc.ToolRequestUserInputResponse`, and the cancel shape `{answers: {}}` (cross-phase decision 9), before step 27.
- Keyword matcher: the server owns it (cross-phase decision 65). Step 10 deletes `keywordSpans` (`internal/tui/keywords.go:59`) and asks `dalgona/keywords/match` (the protocol workstream, Step 7) for the marks.
- Modules: `golang.org/x/text` is a direct requirement at v0.43.0 (first `require` block). `width.go` (Step 2) imports `unicode/norm` from it, and the glyph test (Step 12) imports `golang.org/x/text/width`. Cross-phase decision 25 pins v0.44.0 in the early pin phase; step 2 builds against that pinned version. `github.com/rivo/uniseg` stays out of this workstream, because `width.go` measures with `ansi` (cross-phase decision 57). `github.com/charmbracelet/x/ansi` (v0.11.9, indirect today) becomes direct at step 2, its first import (cross-phase decision 56). `github.com/charmbracelet/colorprofile` (v0.4.3, indirect today) becomes direct at step 9, its first import. `charm.land/bubbles/v2 v2.2.1` is absent today and is added at its first import, in step 11 (cross-phase decision 25). Record each move in `docs/decisions/dependencies.md`.
- Design values: `DESIGN.md` (tests-and-docs workstream, Step 19) holds every colour and icon value (Schemas T1 and T2). Steps 9 and 12 read them from that file, so it must land before both.
- Other workstreams that depend on this workstream: ask Step 7 lands after terminal Step 36, because the question card calls `notify`; edit-review Step 7 lands after terminal Steps 9, 14 and 16; live-debug Step 4 (Terminal session) lands after terminal Step 37.
- Design: the section "Design record" of this plan is the source text for `DESIGN.md`, which the repository does not have yet. This workstream carries the values it needs in tables T1 and T2, so no step waits for `DESIGN.md`. The section "Terminal screen design" and cross-phase decisions 13 to 19 and 26 to 58 govern the terminal steps.
- Live debugging: the omp `tui` tool in `.omp/tools` for the end-to-end check only.

