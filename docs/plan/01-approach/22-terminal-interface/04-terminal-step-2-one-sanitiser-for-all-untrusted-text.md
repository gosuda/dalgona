#### Terminal step 2. Separate safe display from input

This step follows [Input text](../../../dal/spec/input-text.md) and [TUI](../../../dal/spec/tui.md#untrusted-text).
It replaces the earlier ingress-cleaning and NFC measurement instructions.
The implementation paths below are under `internal/dal/tui`.

- Validate committed input before insertion. Reject malformed UTF-8 and forbidden field values without replacement decoding or partial edits.
- Keep accepted scalars in drafts, history, paths, labels, and protocol objects. Preserve private-use characters, noncharacters, joiners, variation selectors, and combining sequences.
- Build a separate safe display projection. Terminal control sequences must never execute outside an actual raw terminal.
- Preserve incomplete output escape sequences across deltas within the TUI's declared pending-state bound. Do not sanitize each fragment independently.
- Show non-layout controls and directional controls as inspectable escapes where the shared contract requires them. LF and TAB retain their declared layout.
- Keep exact source-to-display mappings for selection, copy, caret position, and hit testing. Display escapes must not replace canonical input or selected option values.
- Measure, wrap, and clip with the same declared renderer width profile. Do not apply NFC before or after measurement.
- Clip only at complete editing clusters and complete visible escapes. Rebuild geometry after profile changes without changing source text.
- Keep notices bounded to one row and retain existing error roles and dock-height invariants.
- Apply the same presentation boundary to model output, command output, approvals, login displays, lists, plans, and editable fields.
- Use the [shared vectors](../../../dal/spec/input-text-cases.json) for preservation and offset checks. Test actual terminal profiles for cell geometry separately.
- Verify an escape sequence split at every byte boundary cannot affect terminal state. The original accepted input must remain available through its source copy path.
- Verify decomposed text stays decomposed through layout, copy, history, and send.
- Verify private-use text next to a paste chip cannot address or remove that chip.
- Update the dependency decision only when the implementation changes its actual imports or pins.

These are implementation requirements, not passing test results.
The shared contract's real terminal and IME evidence remains a release gate.

