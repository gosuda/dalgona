#### Web step 32. Thread command panels and the rewind gesture

- New panels in `panels/`:
  - `ResumePanel`: a filterable list from `thread/list` (`limit: 100`; drop the `sortKey` and `sortDirection` members that `+page.svelte:238` sends).
    - Conversation search waits 300 ms and requires at least one Unicode-whitespace-separated term, with at least three scalars in every term.
    - Send the original filter unchanged. Padding does not make `ab ` eligible, and a short term keeps `abc de` local.
    - ` abc ` is eligible and must preserve its spaces in the request. Local filtering remains available for ineligible queries.
  - `NewThreadPanel`: confirms `Start thread`.
  - `CompactPanel`: `Compact this thread? Earlier turns stay in the journal.` with button `Compact`, which calls `thread/compact/start {threadId}`.
  - `ForkPanel`: a turn list from `dalgona/thread/tree`, then `thread/fork {threadId, lastTurnId}`.
  - `TreePanel`: a turn list with `preview` and `Active`, then `dalgona/thread/leaf/set {threadId, turnId}`.
  - `RewindPanel`: pick a user turn, then confirm with `Rewind to before this turn? Later turns leave the active path. Files changed by later turns are not reverted.` and button `Rewind`, which calls `thread/revert {threadId, beforeTurnId}` and then `session.selectThread`.
  - `AdvisorPanel`: status rows, `Turn advisor on` or `Turn advisor off` (`dalgona/advisor/set {threadId, enabled}`), and `Reset advisor` (`dalgona/advisor/reset {threadId}`).
  - `ReviewStartPanel`: target radio `Uncommitted changes`, `Base branch` (text input), `Commit` (text input) or `Custom` (textarea), then `review/start {threadId, target}` (protocol target forms).
- Mid-turn rule. While a turn runs, `/new`, `/compact`, `/fork`, `/tree` and `/revert` show `/${name} is unavailable while a turn is working; wait for it to finish or interrupt it first` with buttons `Interrupt` and `Close`.
- Rewind gesture. Each user `MessageRow` has a `Rewind to here` button, always visible at the end of the row, with no hover-only reveal that opens `RewindPanel` preselected on its `turnId`.
- Tests. `ThreadPanels.dom.test.ts`:
  - `rewind asks before calling thread/revert`
  - `compact is blocked mid-turn with an Interrupt action`
  - `review/start sends the chosen target`
  - `resume search rejects padded or mixed short terms without changing the filter`
  - `resume search preserves an eligible query including surrounding whitespace`
  - Fails before: none of these exists in the web client.
- Spec (`docs/spec/web.md`). Panel rows plus the mid-turn rule.
- Lands after: Steps 16, 18 and 28.

Anchor: J.R.R. Tolkien, Tony Hoare.

