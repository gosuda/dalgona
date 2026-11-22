#### Terminal step 25. `/login` panel

- `login.go`: `panelLogin` (12 rows). `type loginPanel struct{ phase loginPhase; provider string; cursor int; loginID, url, code, err string }`, with the phases `loginProviders`, `loginMethods`, `loginAPIKey` and `loginWaiting`. The panel holds no text field. The key is typed in the input line (`inputAPIKey`, Step 21).
- Providers come from `dalgona/account/providers/list` (dependency L1, the protocol workstream, Step 12). No such method exists in `internal/rpc` today; `account/read` is registered at `internal/rpc/serve.go:40`. Each row shows `Signed in` or `Not signed in` and its methods.
- Methods: a provider with the `apiKey` method opens the API key field. ChatGPT offers the rows `Browser` and `Device code`. They send `{type: "chatgpt"}` or `{type: "chatgptDeviceCode"}` (`internal/rpc/serve_misc.go:205-257`).
- API key: `enter` sends `account/login/start {type: "apiKey", provider, apiKey}` from a `tea.Cmd`, because `Update` does no I/O (Bubble Tea practice rule 6). Success: the panel closes, and the notice of Step 8 shows. Error: the panel shows the error in its body and stays open. `esc` clears the field and returns to the method rows. Nothing is sent.
- Browser and device code: the panel shows the URL and waits for `account/login/completed` (Step 8). Browser shows `authUrl`. Device shows `verificationUrl` and `userCode` (`internal/rpc/types.go:148-150`; the server builds them at `internal/rpc/serve_misc.go:340`). The user enters the device code at the verification page, not in the client. The client takes no typed text for it (cross-phase decision 45). `esc` sends `account/login/cancel {loginId}`.
- `/login <provider>` opens the panel at that provider's methods.
- The key never enters history, the notice, a row or the review. The field is masked, with one `*` per rune. A valid single-line paste fills the field without trimming or normalization and never becomes a chip. A multiline paste is rejected whole with a field error and preserves the previous key. The field is cleared when the panel closes.
- Symbols that change: `loginPanel` (new, `login.go`). `loginCommand` (`slash.go`, Step 1) opens it. `applyLogin` belongs to Step 8.
- Failing-first tests:
  - `TestLoginPanelApiKeyFlow` (from the workstream). Input: type the key, then `enter`. Expected: one `account/login/start` with `type` `apiKey`, `provider` and `apiKey`. Then the notice above. Source: `serve_misc.go:205-257`.
  - `TestLoginPanelOAuthCompletes` (from the workstream). Expected: after `account/login/completed`, the notice `Signed in to chatgpt. Run /model to choose a model`.
  - `TestLoginPanelCancelSendsCancel` (from the workstream). Expected: `account/login/cancel` with the `loginId`.
  - `TestLoginKeyNeverRendered` (from the workstream), extended. Input: type `sk-1234`. Expected: seven `*` on the input line and no `sk-1234` in the view. Source: the masking rule above.
  - `TestLoginPanelSteady` (new). Input: `assertSteadyDock` with 50 `down` presses in the providers phase. Then 50 printable keys in the API key phase. Expected: 15 frame rows (3 plus 12). Source: Step 21 table.
- Spec: `docs/spec/tui.md`, its `/login` subsection under Panels (Step 21 owns the section).
- Lands after: Steps 1, 3, 8 and 21, and the protocol workstream, Step 12.
- Anchor: Tony Hoare

