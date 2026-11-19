#### Prerequisites of the ask workstream

- Steps 1 to 6 need no other workstream. Their order is in the Lands after lines. The protocol workstream, Step 13 needs Steps 2, 3 and 3a first. It reuses the `ToolRequestUserInput*` types, the name `spawnRequest` and the routing of Step 3a.
- Step 7 lands after the terminal workstream, Step 36 (window title and desktop notifications), which creates the notification command (product decision 3). Step 36 creates `notify(text string) tea.Cmd`. It returns nil while notifications are off or the window is focused. It already sends the ask notification, so Step 7 calls no notify of its own. Step 7 uses the Measured ledger row and `commit` from the terminal workstream, Step 14, the Open rail panel and `panelAsk` from step 21, the decision queue from step 26, and `threadHue` from step 9 (Design record, The point and the thread hue). Step 7 adds no fallback rendering.
- Step 8 lands after the web workstream, Steps 4, 8, 10, 21 and 23. Those steps hold the page state, the tokens, the state words, the approval card and the request queue. It reuses the approval card's structure.
- Later workstreams consume this workstream's interface:
  - The ratchet workstream, Step 6 (product decision 10) and the protocol workstream, Step 13 (product decision 12) call `(*agent.Thread).Ask(ctx, callID string, []asktool.Question) (asktool.Reply, error)` and read `Reply.Answers` by question index. The sandbox choice is asked once per thread (the protocol workstream, Step 13), so its caller keeps that answer for the thread; `Ask` keeps no answers.
  - The terminal workstream, Step 11 and the web workstream, Step 10 must map `awaiting input` to `Waiting`. Their current text does not yet.

