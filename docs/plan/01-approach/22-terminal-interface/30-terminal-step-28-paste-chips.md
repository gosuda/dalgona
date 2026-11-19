#### Terminal step 28. Paste chips

- `input.go`: prompt mode only. A `PasteMsg`, sanitised by Step 2, with 10 or more lines (cross-phase decision 13) becomes one atom rune U+F0000+n, where n is the number of chips in the prompt after this one is added. It is stored in `editor.chips` as `chip{label, text string}` and shown as `[Paste #n +L lines]`, where L is the line count. A paste of fewer than 10 lines is inserted as text. The atom rune never reaches the screen: the label is what is drawn, and Step 2 removes private-use runes from untrusted text. The label is measured with `cells`.
- A chip is one row. A chip takes one composer row, whatever its length, so the dock does not grow with a long paste.
- `backspace` and `delete` remove the whole atom. Sending expands the atoms in order.
- A paste over 1 MiB is refused. The notice in the live row is `Paste is <n> bytes, over the 1 MiB limit; save it to a file and mention it with @path`, where `<n>` is the byte count of the paste (cross-phase decision 48).
- A text mode takes the first line of a paste and makes no chip (Step 21).
- Symbols that change: the paste case of `Update` (`composer.go:63-70`, renamed by Step 19) and `editor.chips`.
- Failing-first tests:
  - `TestLargePasteBecomesChipAndExpandsOnSend` (from the workstream). Input: a 12-line paste, then send. Expected: the input shows `[Paste #1 +12 lines]` and the sent text contains all 12 lines.
  - `TestChipDeletesAsUnit` (from the workstream). Expected: one `backspace` removes the whole chip.
  - `TestOversizePasteRefused` (from the workstream). Expected: the notice is the literal above.
  - `TestPasteIntoFilterKeepsFirstLine` (new). Input: a paste of two lines into the filter. Expected: the field text is the first line only. Source: Step 21.
  - `TestChipRowDoesNotGrowComposer` (new). Input: a 40-line paste. Expected: 3 frame rows (composer 1, live 1, band 1). Source: the one-row chip rule above and section "Terminal screen design", I2.
- Spec: one paragraph (paste) in the `Editor` section that Step 19 creates. This step adds no other spec text.
- Reference: oh-my-pi `prompt/composer.ts:448, 457` and grok-build `views/prompt_widget/mod.rs:1853-1858` (via the feature reports). Taken: an atom chip that expands on send. Left: raw-paste key, chip preview.
- Lands after: Steps 2, 3, 19 and 21.
- Anchor: Claude Shannon

