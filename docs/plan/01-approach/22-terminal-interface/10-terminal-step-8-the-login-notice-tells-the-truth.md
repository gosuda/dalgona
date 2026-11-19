#### Terminal step 8. The login notice tells the truth

- `commands.go` `applyLogin` (`commands.go:626-640`): the notice is `Open <url> to sign in` + `; enter code <code>` when present + `; waiting for the provider to confirm`. The notice is one row (step 2).
- `apply.go`: add `account/login/completed` to `applyDecodedNotification` (`model.go:273-293`). Decode `rpc.AccountLoginCompletedNotification` (`internal/rpc/types.go:285-289`). Today that type has `LoginID`, `Success` and `Error`. The `provider` field comes from the protocol workstream, Step 12 (`AccountLoginCompletedNotification.Provider`, and the N1 schema in The schemas of this workstream). On success: `Signed in to <provider>. Run /model to choose a model`. On failure: `errorf("Login failed: %s; run /login again", error)`.
- Test (fails before): `TestLoginCompletedNotice`. Input: the success payload `{"success":true,"provider":"openai"}` gives the notice `Signed in to openai. Run /model to choose a model`. Input: the failure payload `{"success":false,"error":"openai did not accept the API key; it was not saved","provider":"openai"}` gives the notice `Login failed: openai did not accept the API key; it was not saved; run /login again`. The expected texts come from this step and from the protocol Schemas example for `account/login/completed`.
- Spec: `docs/spec/tui.md` Notifications list adds `account/login/completed`.
- Lands after: Steps 1 and 2, and the protocol workstream, Step 12.
- Anchor: Richard Feynman.

