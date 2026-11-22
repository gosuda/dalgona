### Outside checks of the ask workstream

The [ask contract](../../dal/spec/ask.md) and [shared input contract](../../dal/spec/input-text.md) define the current acceptance requirements.
The checks below require external-client or real input-method evidence and remain unverified until recorded against those environments.

- Test Korean, Japanese, and Chinese composition in first-party question fields on the declared OS, browser, and terminal pairs.
- Confirm that composition Enter commits text without also submitting, and composition Escape cancels without dismissing the request.
- Submit an oversized answer, correct it through the retained sender, and verify the same request ID and original deadline.
- Test old clients that close their card after submission. Invalid text must remain rejected without a promise of reconnect recovery.
- Verify source-preserving fallback and inspectable option differences with missing glyphs, formatting controls, and exact offered CRLF labels.

- Check: Live Codex desktop client: send a dismiss of an item/tool/requestUserInput request and record the response shape it returns (an error response, or the result {"answers": {}}). Guards: Step 3 (requestInput error branch and inputReply). Why it is outside: Codex desktop is an outside client; the vendored schema does not say how it dismisses this request. Fallback: If Codex dismisses with an error response, make no change: D5 treats it as cannot answer, so the question declines with declined only when that client is the only subscriber and otherwise stays open for the other windows. If Codex dismisses with {"answers": {}}, D5 already maps it to OutcomeCancelled.

