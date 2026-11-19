#### Live-debug step 4. Write the session scripts, the defect loop and the report rules into `docs/spec/live-debug.md`

File to update: `docs/spec/live-debug.md`, sections "Terminal session", "Browser session", "Defect loop", "Not provable here" and "Report". The content is the five sections named above, copied as normative text.

The failing test before the change is `internal/llmstub` `TestLiveSessionTurnsExist`. It reads the turn ids that the "Terminal session" and "Browser session" tables name, and fails until each exists in `live.json`, and the reverse. Step 2 asserts only that the script parses. The id check belongs here.

- Lands in phase 21, after eval default (phase 16), code search (17), code symbols (18), code rewrite (19), comment removal (20), the terminal workstream, Step 37, the web workstream, Step 38, and Steps 1 to 3 of this workstream. The interface steps in the Dependencies section land before it.

Anchor: Edward Tufte, Italo Calvino.

