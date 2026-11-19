#### Specifications step 25. Correct `docs/spec/providers.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/providers.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 33 | Devin chat MUST send one gzip-compressed Connect frame to `GetChatMessage` with request type 5, planner mode 1, tool choice `auto`, an ephemeral system prompt cache, and parallel tool calls disabled. | Devin chat MUST send one gzip-compressed Connect frame to `GetChatMessage`. The request MUST use request type 5 and planner mode 1. The request MUST set tool choice `auto`, an ephemeral system prompt cache, and `DisableToolCalls` to true. | internal/model/devin/request.go:106 `DisableToolCalls:   true,`; request.go:107 `ToolChoice: ... "auto"`; internal/model/devin/wire/chat.go:300 `DisableToolCalls        bool`; chat.go:331 `dst = appendBool(dst, 11, m.DisableToolCalls)` |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 23 | A token that expires without a refresh token MUST return the error `<provider> token expired and has no refresh token; log in again`. | internal/auth/oauth/source.go:67-68 when the cached Refresh is empty, returns errors.New(s.provider.ID + ` token expired and has no refresh token; log in again`) |
| 25 | The account id comes from the ID token. It is the `chatgpt_account_id` claim under `https://api.openai.com/auth` when present. Otherwise it is the `email` claim, or the `sub` claim when no `email` claim exists. | internal/auth/oauth/oauth.go:685 `Account: jwtAccount(fields.idToken),`; oauth.go:695-709 jwtAccount reads `chatgpt_account_id` under `https://api.openai.com/auth`, else `claims.firstString("email", "sub")`; internal/mod |

Use one word for one thing.

- Keep `reasoning`. Replace thinking at lines 49.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 33 | Devin chat MUST send one gzip-compressed Connect frame to `GetChatMessage`. The request MUST use request type 5 and planner mode 1. The request MUST set tool choice `auto`, an ephemeral system prompt cache, and `DisableToolCalls` to true. |
| 65 | When Google returns a function call without an id, the decoder MUST generate an id with a decoder-unique prefix. When Google returns an id that the decoder already saw, the decoder MUST generate an id with a decoder-unique prefix. |
| 67 | An SSE read that ends on cancel, start timeout, or idle timeout MUST close the body. The read MUST wait for its reader goroutine before it returns. |
| 79 | Transient phrases such as `timeout` MUST match only local transport error text. They MUST NOT match a provider body. Model text in a refusal therefore cannot cause a retry. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 4 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/providers.md` prints `style findings: 0`, and `just verify` passes.

