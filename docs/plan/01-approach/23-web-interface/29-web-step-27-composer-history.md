#### Web step 27. Composer history

- Session. `history: readonly string[]` holds the user message texts of the open thread in order, plus texts sent during the session.
- Composer keys.
  - ArrowUp with the caret at offset 0 and no selection recalls the previous entry.
  - ArrowDown at the end recalls the next entry, and finally the draft that was saved on the first ArrowUp.
- Test. `Composer.dom.test.ts`: `ArrowUp at the start recalls the last message and ArrowDown restores the draft`. Fails before: there is no history.
- Spec (`docs/spec/web.md`). `### Composer` keys table. This step creates the table, and Steps 25, 26, 33, 34 and 35 add their own rows.
- Lands after: Steps 4 and 25.

Anchor: J.R.R. Tolkien.

