#### Terminal step 24. `/settings` panel

- `settings.go`: `type settingsPanel struct{ focus, statusItem int; saved, preview map[string]string; notice string }`. The panel holds no text. `saved` holds the values on disk and `preview` holds the values on screen, so that `esc` can restore them.
- Seven rows, in this order (cross-phase decision 28): `Approval` (`ask`, `auto`, `yolo`; key `approval`), `Verbosity` (`low`, `medium`, `high`; key `verbosity`), `Theme` (`auto`, `auto-soft`, `day`, `soft-day`, `night`, `soft-night`; key `theme`), `Icons` (`auto`, `unicode`, `nerd`; key `icons`), `Status` (the seven hideable items of Design record, Status band, each on or off; key `status_hide`, wire name `statusHide`), `Edit review` (`off`, `on`, default `off`; key `edit_review.enabled` (the edit-review workstream, Step 3), wire name `editReview`), and `Notifications` (`on` and `off` on screen; key `notifications`, wire name `notifications`, a boolean with default `true`, cross-phase decision 49).
- There is no sandbox row and no effort row (cross-phase decisions 28 and 46). The sandbox is set in the config file or when a thread starts, because a saved value does not change a running thread (cross-phase decision 30). Effort stays in the model picker (Step 21).
- Keys: `up` and `down` move the focus. `left` and `right` cycle the value of a value row. On `Status`, `left` and `right` move between the seven items, and `space` toggles the focused item. On `Edit review` and `Notifications`, `space` toggles the value.
- Preview: theme, verbosity and icons take effect at once while the panel is open. `esc` restores the saved values and closes the panel.
- Detail block: 7 rows. It is blank unless `Icons` or `Status` has focus. On `Icons` it shows a header with the columns `Unicode` and `Nerd` and six rows: effort, approval, sandbox, folder, queue and context. The glyphs follow table T2, with the Unicode column of cross-phase decision 18. On `Status` it shows the seven items as `[x]` or `[ ]` and the item name.
- `enter` saves the changed rows in one `config/batchWrite {edits: [{keyPath, value, mergeStrategy: "replace"}]}`, all or none (cross-phase decision 11). The keyPaths are `approval`, `verbosity`, `theme`, `icons`, `status_hide`, `edit_review.enabled` and `notifications`. `status_hide` is an array of item ids. `model` is never offered.
- Success: the notice `Saved to <filePath>` is shown and the panel closes. When `approval` changed, the notice is `saved; open threads keep their approval and sandbox` (cross-phase decision 30). The notice shows in the live row.
- Error: the panel stays open and the message shows in the live row (`errorf`, Step 2).
- Symbols that change: `settingsPanel` (new, `settings.go`). `func (m *model) settingsKey(key string) tea.Cmd` handles the keys. `func (m *model) saveSettings() tea.Cmd` builds the batch. `commands` (Step 7) gains `settings` with the summary `Show settings`.
- Failing-first tests:
  - `TestSettingsPanelSavesChangedRows` (from the workstream). Input: change `approval` to `auto`, then `enter`. Expected: one batch with only the keyPath `approval` and value `auto`. Source: decision 11 of this workstream.
  - `TestSettingsPanelKeepsOpenOnError` (from the workstream). Input: the fake caller fails with `disk full`. Expected: the panel is open and the notice contains `disk full`.
  - `TestSettingsEscRevertsPreview` (from the workstream). Input: set theme to `day`, then `esc`. Expected: the view uses the saved theme.
  - `TestSettingsSavesIconsAndStatusHide` (from the workstream, unchanged). Input: toggle `folder` and `mark` on, set icons to `nerd`, then `enter`. Expected: one batch with `status_hide` `["folder","mark"]` and `icons` `"nerd"`.
  - `TestSettingsApprovalNotice` (new). Input: change `approval` and save. Expected: the notice is `saved; open threads keep their approval and sandbox`. Source: cross-phase decision 30.
  - `TestSettingsRowsInOrder` (new). Expected: the rows read Approval, Verbosity, Theme, Icons, Status, Edit review, Notifications. Source: decision 28 of this workstream.
  - `TestSettingsDockSteady` (new). Input: `assertSteadyDock` with 50 `left` and `right` presses on `Status`, then 50 `down` presses. Expected: 20 frame rows (3 plus 17). Source: Step 21 table.
- Spec: `docs/spec/tui.md`, its `/settings` subsection under Panels (Step 21 owns the section).
- Lands after: Steps 3, 7, 9, 12, 16 and 21, and the protocol workstream, Steps 8 and 10.
- Anchor: Dieter Rams

