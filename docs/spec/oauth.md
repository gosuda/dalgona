# OAuth flows

The OAuth package MUST expose provider-neutral authorization flows. It MUST NOT import `internal/auth`. The `Providers` table MUST include ChatGPT, Claude, Google Antigravity, Devin, and Cursor. Each entry MUST use the client identifiers, scopes, endpoints, and parameter names of its named donor.

`Provider` describes authorization, token, device, poll, and refresh endpoints. It also describes client credentials, scopes, loopback binding, advertised redirect URIs, port fallback, and extra authorization parameters. `PollURL` and `RefreshURL` MAY differ from `TokenURL`.

## Authorization code with PKCE

`PKCE` MUST generate a random verifier with at least 256 bits of entropy. It MUST derive the challenge as unpadded base64url of SHA-256 of the verifier. It MUST send `code_challenge_method=S256`. It MUST generate a random state. Claude MUST use the verifier as the state, as its donor requires.

The callback listener MUST bind the configured host and primary port. It MUST try the configured fallback port when the primary bind fails. Claude MUST try an ephemeral port as its fallback. The advertised redirect URI MUST use `RedirectHost`, the bound port, and `RedirectPath`. ChatGPT MUST use `/auth/callback`.

The package MUST open the authorization URL after the listener starts. The loopback callback MUST require an exact state match. A mismatch MUST fail with `OAuth state mismatch`. The callback MUST NOT exchange a code after a mismatch. A successful callback MUST return plain text `Login complete. You can close this tab.`.

The paste path MUST accept a complete redirect URL, a `code#state` value, a query string, or a bare code. A pasted state MUST match when present. A bare code carries no state and MUST follow the donor's manual-code path. An empty pasted code MUST fail. The authorization wait MUST time out after two minutes. Context cancellation MUST stop the wait and the callback server.

## Token responses

`Token` contains access and refresh tokens, an expiry time, and an account identifier. A refresh response without a refresh token MUST retain the previous refresh token. An `expires_in` value MUST set the expiry relative to the exchange time. Cursor and Devin tokens MAY use the JWT `exp` claim when no `expires_in` field exists.

ChatGPT account extraction MUST read `chatgpt_account_id` below the `https://api.openai.com/auth` claim in the ID token payload. The package MAY decode the unsigned JWT payload with base64url and `jsontext`. This read extracts metadata only. It MUST NOT count as signature, issuer, audience, or expiry validation. The provider access token remains the authority.

Token exchanges MUST reject non-success HTTP status codes, malformed JSON, non-object JSON, trailing JSON values, missing access tokens, and responses larger than 1 MiB. Each HTTP request MUST time out after 30 seconds. TLS certificate verification MUST remain enabled.

ChatGPT and Antigravity MUST use form-encoded authorization-code exchanges. Claude MUST use a JSON exchange that includes `state`. Devin MUST use a JSON exchange containing only `code` and `code_verifier`. Devin MUST read `token` as both tokens when no separate refresh token exists. Antigravity MUST include its donor client secret.

`Refresh` MUST use the refresh-token grant. ChatGPT, Claude, and Cursor MUST send JSON refresh requests. Other providers MUST send form-encoded refresh requests. Cursor MUST use `RefreshURL`, not `PollURL`. Cursor MUST reject a response with `shouldLogout=true`, including an HTTP 200 response.

## Device authorization

`DeviceCode` MUST call the user-supplied display function with the user code and verification URL. A missing provider verification URL MAY come from `DeviceVerificationURL`. Polling MUST stop on context cancellation. The whole flow MUST stop after 15 minutes.

The standard device flow MUST use the RFC device-code grant. It MUST default to a five-second interval when the response omits `interval`. It MUST continue after `authorization_pending`. It MUST add five seconds to the interval after `slow_down`. It MUST accept pending and slow-down responses with non-success HTTP statuses. Any other OAuth error MUST fail the flow.

ChatGPT MUST use its donor device endpoints instead of the RFC grant. The initial JSON request MUST contain `client_id`. The poll JSON request MUST contain `device_auth_id` and `user_code`. HTTP 403 and 404 poll responses MUST mean pending. A successful poll MUST supply an authorization code and verifier. The package MUST exchange them at `TokenURL` with `DeviceRedirectURL`.

## Cursor deep link

`CursorPoll` MUST generate a PKCE verifier and a random RFC 4122 version 4 UUID. The deep link MUST contain `challenge`, `uuid`, `mode=login`, and `redirectTarget=cli`. The verifier MUST NOT appear in the deep link.

The poll MUST be a GET to `PollURL` with `uuid` and `verifier`. HTTP 404 MUST mean pending. The initial delay MUST be one second. Each pending response MUST multiply the delay by 1.2, up to ten seconds. Polling MUST stop after 150 attempts. Three consecutive transport, HTTP, or decoding failures MUST fail the flow. A successful response MUST read `accessToken` and `refreshToken`.
