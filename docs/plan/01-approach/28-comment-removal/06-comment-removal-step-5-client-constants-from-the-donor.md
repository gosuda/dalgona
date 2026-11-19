#### Comment removal step 5. Client constants from the donor files

Anchor: Richard Feynman

- Files: `internal/auth/oauth/oauth.go` (constants at lines 57 and 59) and `internal/auth/oauth/oauth_test.go`.
- Change: set `anthropicPublicID` (line 57) to the text that base64 decoding gives for the argument of `decode(...)` on line 14 of `.references/pi/packages/ai/src/auth/oauth/anthropic.ts`. Set `antigravityPublicKey` (line 59) to the text that base64 decoding gives for the `client-secret` value on line 7 of `.references/oh-my-pi/packages/catalog/src/compat/rules/auth/google-antigravity.kdl`. Change no other constant. The decoded values are written only into these two constants, never into the plan or another file.
- Tests that fail before the change: `TestClientSecretsDecodeFromDonors` in `internal/auth/oauth/oauth_test.go`, table-driven, with the columns name, encoded, got and want. Row `anthropic client id`: encoded is the base64 string on `anthropic.ts` line 14, copied as a literal; got is `anthropicPublicID`; want is `base64.StdEncoding` decoding of encoded, computed in the test. Source of the expected value: the donor's base64 encoding, an independent oracle. Row `antigravity client secret`: encoded is the `client-secret` string on `google-antigravity.kdl` line 7, copied as a literal; got is `antigravityPublicKey`; want is its decoding. Both rows fail before the change because the code values differ from the donor values.
- Check: `go test -race -count=1 ./internal/auth/oauth/` passes.
- Order inside the step: make the change in the working tree, run the test, build both products, and run the live sign-in of outside check 1 for both providers on that build. Commit only when both sign-ins succeed. The owner chose this order on 2026-11-12.
- Fallback: if a donor file or line is missing, stop this step and report it. `.references/` is git-ignored and absent from a clean clone, so the step runs only on a host that has it. If a sign-in fails, outside check 1 gives the action.
- Lands after: Comment removal step 4

