#### Edge cases (ask workstream)

- If the model's arguments fail validation, the call completes `ok=false` with the `ask:` message, and the loop guard (4 identical calls) still applies.
- A plugin hook can deny `ask` like any other call. In mode `auto` or `ask`, class `read` accepts mechanically (`docs/spec/approvals.md` step 3).
- If the resolved notification reaches a window before its request, the late card stays open until the user answers or dismisses it. The answer is then dropped as unknown, and the transcript item shows the real answer.
- A steer or follow-up typed in the browser while a question is open is delivered by the existing steer rules once the tool returns. In the terminal, the card holds the keys.
- If the thread closes during a wait, the outcome is interrupted. If the completion append then fails, recovery applies the D3 row.
- Typed answers pass through the secret masker like any tool output (`tools.go:417-420`).
- Two `ask` calls in one assistant message run one after the other, since only class `agent` runs in batches (`tools.go:101-111`), so a thread never has two questions open.

