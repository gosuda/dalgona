#### Web step 31. Login panel

- New `panels/LoginPanel.svelte`. Providers are a radio list from `dalgona/account/providers/list`; `terminal` providers show `<product> login <id>` only. The method depends on the provider:
  - API key: `<input type="password" autocomplete="off" spellcheck="false">` labelled `API key for ${provider}`. Calls `account/login/start {type: "apiKey", provider, apiKey}`. Empty input shows `Enter the API key.`
  - Browser: `account/login/start {type: "chatgpt"}` returns `authUrl` and `loginId`. Show the link `Open sign-in page`, the line `Waiting for sign-in…` and `Cancel`, which calls `account/login/cancel {loginId}`.
  - Device code: `account/login/start {type: "chatgptDeviceCode"}`. Show the verification URL and the user code in mono with `Copy code`.
- Session. Handles `account/login/completed {success, error, provider}`, plus `loginId` for browser and device flows.
  - Success: notice `Signed in to ${provider}`, then reload `model/list` and close the panel.
  - API key: the `error` shows under the key field, and the panel stays open. Browser and device: an error in `NoticeStack`'s alert region (Step 5) `Login failed: ${error}; run /login again` with `Try again`.
  - Add `account/login/completed` to `parseNotification` and to the conformance table (`AccountLoginCompletedNotification.json`).
- Test. `LoginPanel.dom.test.ts`:
  - `an API key login sends provider and key`
  - `a browser login waits for account/login/completed and reloads models`
  - `a failed login keeps the panel open with Try again`
  - Fails before: neither client consumes `account/login/completed`.
- Spec (`docs/spec/web.md`). Login row in `### Commands and panels`.
- Lands after: Steps 5 and 28.

Anchor: Milton Erickson.

