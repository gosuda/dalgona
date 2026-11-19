#### Comment removal step 20. Web client comment

Anchor: Saul Bass

- Files: `web/src/routes/+page.svelte`, `docs/spec/web.md`.
- Change: delete the comment at `web/src/routes/+page.svelte:166`. The code line `selectionGeneration += 1;` stays.
- Spec text, `docs/spec/web.md`: as a new bullet directly after the bullet that starts `Selecting Reconnect loads the thread list`, under `### Empty states and errors`: A new connection MUST discard thread resumes still in flight from the previous connection.
- Tests that fail before the change: none. The behaviour does not change, and `web-check` covers the web package.
- Check: `just verify` passes. `grep -n "discard thread resumes" docs/spec/web.md` returns one line. `grep -n '^[[:space:]]*//' web/src/routes/+page.svelte` returns no line.
- Lands after: Comment removal step 19

