# OAuth flows

The `chatgpt` entry MUST take its client values from Codex `codex-rs/login/src/server.rs`. The `anthropic` entry MUST take its client values from `pi`'s `packages/ai/src/auth/oauth/anthropic.ts`. The `antigravity` entry MUST take its values from `oh-my-pi`'s `packages/catalog/src/compat/rules/auth/google-antigravity.kdl`. The `devin` entry MUST take its values from `oh-my-pi`'s `packages/catalog/src/compat/rules/auth/devin.kdl`. The `cursor` entry MUST take its values from `oh-my-pi`'s `packages/ai/src/registry/oauth/cursor.ts`.

The OAuth package (`internal/auth/oauth`) MUST expose provider-neutral authorization flows. It MUST NOT import `internal/auth`. The `Providers` table MUST include ChatGPT, Claude, Google Antigravity, Devin, and Cursor. Each entry MUST use the client identifiers, scopes, endpoints, and parameter names of its named donor.

The table MUST also include Copilot.
The `copilot` entry takes its client values from jcode's `crates/jcode-base/src/auth/copilot.rs`.

`Provider` describes authorization, token, device, poll, and refresh endpoints. It also describes client credentials, scopes, loopback binding, advertised redirect URIs, port fallback, and extra authorization parameters. `PollURL` and `RefreshURL` MAY differ from `TokenURL`.

## Authorization code with PKCE

`PKCE` MUST generate a random verifier with at least 256 bits of entropy. It MUST derive the challenge as unpadded base64url of SHA-256 of the verifier. It MUST send `code_challenge_method=S256`. It MUST generate a random state. Claude MUST use the verifier as the state, as its donor requires.

The callback listener MUST bind the configured host and primary port. It MUST try the configured fallback port when the primary bind fails. Claude MUST try an ephemeral port as its fallback. The advertised redirect URI MUST use `RedirectHost`, the bound port, and `RedirectPath`. ChatGPT MUST use `/auth/callback`. If every callback port fails to bind, the flow MUST fail with an `oauth callback listen` error.

The package MUST open the authorization URL after the listener starts. The loopback callback MUST require an exact state match. The loopback callback MUST answer a state mismatch with HTTP 400 `invalid oauth callback`. The loopback callback MUST keep waiting after a mismatch. The paste path MUST fail a state mismatch with `OAuth state mismatch`. The callback MUST NOT exchange a code after a mismatch. A successful callback MUST return plain text `Login complete. You can close this tab.`.

The paste path MUST accept a complete redirect URL, a `code#state` value, a query string, or a bare code. A pasted state MUST match when present. A bare code carries no state and MUST follow the donor's manual-code path. An empty pasted code MUST fail. The authorization wait MUST time out after two minutes. Context cancellation MUST stop the wait and the callback server.

## Token responses

`Token` contains access and refresh tokens, an expiry time, and an account identifier. A refresh response without a refresh token MUST retain the previous refresh token. An `expires_in` value MUST set the expiry relative to the exchange time. A token without an `expires_in` field MAY take its expiry from the JWT `exp` claim.

ChatGPT account extraction MUST read `chatgpt_account_id` below the `https://api.openai.com/auth` claim in the ID token payload. The package MAY decode the unsigned JWT payload with base64url and `jsontext`. This read extracts metadata only. It MUST NOT count as signature, issuer, audience, or expiry validation. The provider access token remains the authority. For a token without `chatgpt_account_id`, the account MUST fall back to the ID token `email` claim, then to `sub`.

Token exchanges MUST reject non-success HTTP status codes, malformed JSON, non-object JSON, trailing JSON values, missing access tokens, and responses larger than 1 MiB. Each HTTP request MUST time out after 30 seconds. TLS certificate verification MUST remain enabled.

ChatGPT and Antigravity MUST use form-encoded authorization-code exchanges. Claude MUST use a JSON exchange that includes `state`. Devin MUST use a JSON exchange containing only `code` and `code_verifier`. Devin MUST read `token` as both tokens when no separate refresh token exists. Antigravity MUST include its donor client secret.

`Refresh` MUST use the refresh-token grant. ChatGPT, Claude, and Cursor MUST send JSON refresh requests. Other providers MUST send form-encoded refresh requests. Cursor MUST use `RefreshURL`, not `PollURL`. Cursor MUST reject a response with `shouldLogout=true`, including an HTTP 200 response.

## Device authorization

`DeviceCode` MUST call the user-supplied display function with the user code and verification URL. A missing provider verification URL MAY come from `DeviceVerificationURL`. Polling MUST stop on context cancellation. The whole flow MUST stop after 15 minutes.

The standard device flow MUST use the RFC device-code grant. It MUST default to a five-second interval when the response omits `interval`. It MUST continue after `authorization_pending`. It MUST add five seconds to the interval after `slow_down`. It MUST accept pending and slow-down responses with non-success HTTP statuses. Any other OAuth error MUST fail the flow.

Copilot MUST use the standard device flow with scope `read:user`.
Its client id MUST be `Iv1.b507a08c87ecfe98`.
Its device endpoint MUST be `https://github.com/login/device/code`.
Its verification URL MUST be `https://github.com/login/device`.
Its token endpoint MUST be `https://github.com/login/oauth/access_token`.
Copilot login MUST print `Open <url> and enter the code <code> to log in to copilot.`.
Copilot MUST NOT use PKCE.
A GitHub token without an expiry MUST NOT refresh.
The `--device-auth` flag remains exclusive to ChatGPT.

ChatGPT MUST use its donor device endpoints instead of the RFC grant. The initial JSON request MUST contain `client_id`. The poll JSON request MUST contain `device_auth_id` and `user_code`. HTTP 403 and 404 poll responses MUST mean pending. A successful poll MUST supply an authorization code and verifier. The package MUST exchange them at `TokenURL` with `DeviceRedirectURL`.

## Cursor deep link

`CursorPoll` MUST generate a PKCE verifier and a random RFC 4122 version 4 UUID. The deep link MUST contain `challenge`, `uuid`, `mode=login`, and `redirectTarget=cli`. The verifier MUST NOT appear in the deep link.

The poll MUST be a GET to `PollURL` with `uuid` and `verifier`. HTTP 404 MUST mean pending. The initial delay MUST be one second. Each pending response MUST multiply the delay by 1.2, up to ten seconds. Polling MUST stop after 150 attempts. Three consecutive transport, HTTP, or decoding failures MUST fail the flow. A successful response MUST read `accessToken` and `refreshToken`.

## Terminal login

The `account/login/start` method MUST refuse an OAuth provider other than `chatgpt`. The error MUST read `login for <provider> runs in the terminal: <product> login <provider>`, where `<product>` is the product name of the host, `dal` or `dalgona`.

A provider in both provider lists MUST report login mode `hybrid`. The sign-in panel MUST offer the API key path and the OAuth shell command for a `hybrid` provider. Without `--api-key` and without `--remote`, `login anthropic` MUST run the OAuth code flow of the pi donor.

## MCP discovery and resource binding

MCP OAuth applies to dalgona HTTP servers only.
`DiscoverResource` uses the `resource_metadata` challenge URL when present.
Otherwise it tries the path-specific protected-resource metadata URL, then the root URL.
The metadata resource MUST equal the canonical MCP URL.
Discovery uses the first authorization server.
HTTP endpoints MUST use HTTPS, except on loopback hosts.

`DiscoverServer` tries OAuth authorization-server metadata before OpenID metadata.
The returned issuer MUST equal the requested issuer.
The server MUST advertise PKCE `S256`.
Dynamic registration requests a public client named `dalgona` with authorization-code and refresh-token grants.
Without a configured client identifier, a server without registration support MUST fail.

The authorization request, code exchange, and refresh MUST carry the canonical resource URL.
Configured scopes override resource metadata scopes.
Neither source means that requests omit scope.
MCP credentials use `mcp-<name>` in the auth store.
Their account MUST equal the canonical resource URL.
Registration metadata lives in `mcp-oauth.json` with mode 0600.
Only login writes registration metadata.
The token source reloads registration before token access and refresh.
Resource and configured client mismatches MUST fail before credentials leave the host.
