# Copilot provider

## Invariant

Both products offer Copilot through explicit GitHub device login. The GitHub credential stays in the auth store. The provider exchanges it for a short-lived Copilot bearer token and shares the cached exchange between callers. Refusals do not start login, rotate credentials, change editor identities, or select another provider.

## Scope

The device client id and endpoints follow `.references/jcode/crates/jcode-base/src/auth/copilot.rs:40-45`. Its editor and integration headers appear at lines 98-101. Its expiry margin appears at lines 137-141, and its exchange authorization appears at lines 633-643. The adapted protocol has the MIT attribution in `THIRD_PARTY_NOTICES.md`.

The client id `Iv1.b507a08c87ecfe98` identifies the VS Code Copilot public client. Its terms for use by another editor are unclear. Acceptance of the `dalgona/<version>` editor identity is unverified and requires an authorized live account check. Local tests do not establish subscription eligibility or provider permission. Real sign-in and editor-identity acceptance remain Not proven.

Copilot advertises no image capability and has no static role preset. The first offered model supplies `smol` and `slow`. HTTP 403 and 404 do not establish that a subscription is missing. Provider diagnostics are bounded and masked before exposure.

## Enforcement

`TestCopilotDeviceLoginStoresGitHubToken` and `TestCopilotDeviceLoginDenialStoresNothing` exercise local device endpoints and auth persistence. `TestLoginDeviceAuthRejectsCopilot` preserves the ChatGPT-only flag. `TestRegistryOffersCopilotOnlyWithACredential` checks application registration.

`TestCopilotHeaders`, `TestCopilotModelsFiltersPicker`, and `TestCopilotExchangeRefusals` exercise local HTTP servers. The token-source tests cover expiry, account changes, shared refresh, timeout, and cancellation. The RPC login tests preserve terminal-only OAuth login routing.

## Rejected alternatives

- Automatic login after an exchange refusal: it cannot determine the account restriction safely.
- Pretending to be another editor after a refusal: it bypasses the declared identity rather than diagnosing access.
- Static model lists: they do not reflect the subscriber's offered models.
