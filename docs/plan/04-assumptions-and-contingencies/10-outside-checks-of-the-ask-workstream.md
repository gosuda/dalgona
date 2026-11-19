### Outside checks of the ask workstream

- Check: Live Codex desktop client: send a dismiss of an item/tool/requestUserInput request and record the response shape it returns (an error response, or the result {"answers": {}}). Guards: Step 3 (requestInput error branch and inputReply). Why it is outside: Codex desktop is an outside client; the vendored schema does not say how it dismisses this request. Fallback: If Codex dismisses with an error response, make no change: D5 treats it as cannot answer, so the question declines with declined only when that client is the only subscriber and otherwise stays open for the other windows. If Codex dismisses with {"answers": {}}, D5 already maps it to OutcomeCancelled.

