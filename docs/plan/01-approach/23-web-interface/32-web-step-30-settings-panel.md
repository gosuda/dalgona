#### Web step 30. Settings panel

- New `panels/SettingsPanel.svelte`. Fieldsets:
  - `Approval`: `ask`, `auto`, `yolo` (config `approval`).
  - `Verbosity`: Low, Medium, High.
  - `Theme`: Auto, Auto soft, Day, Soft day, Night, Soft night (the values of `themeSettings`, Step 8).
  - `Hidden status items`: one checkbox per Design record, Status band item except `model`. A checked box puts its id in the wire list `statusHide`.
  - `Edit review`: one checkbox, `Review edits`, for the wire key `editReview`.
  - `Desktop notifications`: the row only. Step 36 owns its state text and its button.
- Save. `config/batchWrite` sends only changed keys: `theme`, `verbosity`, `approval`, `status_hide` and `edit_review.enabled`, the file key paths of cross-phase decision 27.
  - Validate each value against its enum, with error `Choose a ${label}.`
  - On success, apply the theme at once (`applyTheme`), reload `dalgona/settings/read` and close. A save that changes `approval` shows `saved; open threads keep their approval and sandbox` (cross-phase decision 30).
- Test. `SettingsPanel.dom.test.ts`:
  - `saves only changed keys`
  - `applies the theme after save`
  - `rejects an empty choice with a message`
  - Fails before: there are no settings.
- Spec (`docs/spec/web.md`). Settings panel row in `### Commands and panels`. `### Theme`, which Step 8 creates, gains one line: the setting path `theme` in the Settings panel.
- Lands after: Steps 8 and 28.

Anchor: Dieter Rams.

