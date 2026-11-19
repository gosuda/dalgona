#### Prerequisites of the comment-removal workstream

- Steps land in order, 1 to 21. Each commit passes `just verify`.
- Step 3 lands before any workstream that calls `rules.CheckValue`. No other text in the plan calls it; the protocol and edit-review workstreams use `rules.CheckRemoteValue` when they call the rule.
- The Specifications workstream rewrites `docs/spec` in ASD-STE100. If it lands first, its rewrite keeps the rules of Steps 2 to 20, and the Archive order of Step 2.
- The terminal workstream owns its code blocks. This workstream deletes comment lines, makes the Step 3 rename and changes no other code.
- Any workstream that adds Go, hand-written TypeScript, Svelte or Python code adds no prose comment. Step 21 fails such a commit.
- Step 5 needs `.references/pi` and `.references/oh-my-pi` on the host. Those checkouts are git-ignored and absent from a clean clone.
- No new module dependency. `docs/decisions/dependencies.md` needs no row. The Step 21 test uses the standard library only.
- `THIRD_PARTY_NOTICES.md` does not exist yet. The first porting step creates it (Decision 27).

