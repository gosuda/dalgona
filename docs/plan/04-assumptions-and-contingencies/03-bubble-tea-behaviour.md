### Bubble Tea behaviour

- The terminal relies on `tea.Println` writing above the frame without a clear, and on the renderer erasing from the last row of the last view on exit (`bubbletea/v2@v2.1.1/cursed_renderer.go:201`, `:771-830`). The byte-stream tests of the terminal workstream prove both for the pinned version. If a later Bubble Tea release changes either, keep the pin and report the failing test.
- The one emulator in the module cache is `x/vt`, which the print emulator test uses (Step 3). A screen state after a resize is checked in the live sessions (scenarios T41 and T42), not in a unit test. If a live scenario shows a duplicated or a missing row, record the terminal name and the byte stream (the `raw` capture of the `tui` tool, as scenario T44 uses it), write a byte-stream test that fails, and fix the dock rule that the test names.
- A terminal that draws East Asian Ambiguous characters two cells wide needs `RUNEWIDTH_EASTASIAN=1` (cross-phase decision 57). Without it a row that holds such characters can be wider than measured. `docs/spec/tui.md` states this limit.

