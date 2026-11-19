#### Specifications step 23. Correct `docs/spec/oauth.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/oauth.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 13 | A mismatch MUST fail with `OAuth state mismatch`. The callback MUST NOT exchange a code after a mismatch. | The loopback callback MUST answer a state mismatch with HTTP 400 `invalid oauth callback`. The loopback callback MUST keep waiting after a mismatch. The paste path MUST fail a state mismatch with `OAuth state mismatch`. The callback MUST NOT exchange a code after a mismatch. | internal/auth/oauth/oauth.go:151-152 `if code == "" \|\| subtle.ConstantTimeCompare([]byte(state), []byte(expectedState)) != 1 {` / `http.Error(writer, "invalid oauth callback", http.StatusBadRequest)` / `return`; oauth.go:305-306 `if subtle.ConstantTimeCompare( |
| 19 | Cursor and Devin tokens MAY use the JWT `exp` claim when no `expires_in` field exists. | A token without an `expires_in` field MAY take its expiry from the JWT `exp` claim. | internal/auth/oauth/oauth.go:689-690 `} else {` / `token.Expires = jwtExpiry(fields.access)`; the branch has no provider test and applies to every provider |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 11 | If every callback port fails to bind, the flow MUST fail with an `oauth callback listen` error. | internal/auth/oauth/oauth.go:172 `func listenCallback(provider Provider, host string) (net.Listener, error) {`; oauth.go:185 `return nil, fmt.Errorf("oauth callback listen: %w", errors.Join(listenErrors...))`; oauth.go:1 |
| 21 | For a token without `chatgpt_account_id`, the account MUST fall back to the ID token `email` claim, then to `sub`. | internal/auth/oauth/oauth.go:695 `func jwtAccount(raw string) string {`; oauth.go:709 `account, _ := claims.firstString("email", "sub")`; oauth.go:675-693 tokenFromObject passes fields.idToken to jwtAccount |

Structure:

- Listen failure: add the sentence after line 11 (see add).

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/oauth.md` prints `style findings: 0`, and `just verify` passes.

