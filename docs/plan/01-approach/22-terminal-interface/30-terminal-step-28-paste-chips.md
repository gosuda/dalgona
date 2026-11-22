#### Terminal step 28. Paste chips

- `input.go`: prompt mode only. Validate the complete paste and apply the shared CRLF/CR-to-LF conversion once. A paste of 10 or more lines becomes a tagged chip with identity outside Unicode text. Preserve its accepted text, including final LF. A shorter paste is inserted as one text transaction. No private-use scalar or noncharacter acts as a chip sentinel. Measure only its display label with the shared width profile.
- A chip is one row. A chip takes one composer row, whatever its length, so the dock does not grow with a long paste.
- `backspace` and `delete` remove the whole atom. Sending expands the atoms in order.
- A paste over 1 MiB of incoming UTF-8 is refused without changing the draft or chips. The notice in the live row is `Paste is <n> bytes, over the 1 MiB limit; save it to a file and mention it with @path`, where `<n>` is the byte count of the paste (cross-phase decision 48).
- A one-line text field rejects multiline paste atomically and makes no chip. Preserve its previous value, selection, and recoverable candidate (Step 21).
- Symbols that change: the paste case of `Update` (`composer.go:63-70`, renamed by Step 19) and `editor.chips`.
- Failing-first tests:
  - `TestLargePasteBecomesChipAndExpandsOnSend` (from the workstream). Input: a 12-line paste, then send. Expected: the input shows `[Paste #1 +12 lines]` and the sent text contains all 12 lines.
  - `TestChipDeletesAsUnit` (from the workstream). Expected: one `backspace` removes the whole chip.
  - `TestOversizePasteRefused` (from the workstream). Expected: the notice is the literal above.
  - `TestPasteIntoFilterRejectsMultiline`: paste two lines into a populated filter. The prior value and selection remain, with a visible error and no partial insertion.
  - `TestPrivateUseTextCannotReferenceChip`: paste supplementary private-use text beside a chip. Copy, deletion, and sending preserve the original scalars and the independent chip identity.
  - `TestChipPreservesFinalLineBreak`: expand, recall, and send a chip whose accepted text ends in LF. Each path retains that LF.
  - `TestChipRowDoesNotGrowComposer` (new). Input: a 40-line paste. Expected: 3 frame rows (composer 1, live 1, band 1). Source: the one-row chip rule above and section "Terminal screen design", I2.
- Spec: one paragraph (paste) in the `Editor` section that Step 19 creates. This step adds no other spec text.
- Reference: oh-my-pi `prompt/composer.ts:448, 457` and grok-build `views/prompt_widget/mod.rs:1853-1858` (via the feature reports). Taken: an atom chip that expands on send. Left: raw-paste key, chip preview.
- Lands after: Steps 2, 3, 19 and 21.
- Anchor: Claude Shannon

