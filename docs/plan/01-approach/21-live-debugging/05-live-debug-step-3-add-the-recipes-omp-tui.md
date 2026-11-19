#### Live-debug step 3. Add the recipes `omp-tui-install`, `live-up`, `live-down` and `live-check`

Files to update:

- `justfile`: add the four recipes and the variable `live_root := "/tmp/dg"` (full bodies in the section "Schemas of the live-debug workstream"), appended at the end of the file after the `dist` recipe. The ratchet workstream, Step 3, edits `dist` in place, and this step does not touch it. `verify` is unchanged.
- `docs/spec/live-debug.md`: sections "Run recipe", "Stopping" and "tui tool".
- `CONTRIBUTE.md`: one line under the heading `## Prove the behavior`: `Live debugging: just live-up, then follow docs/spec/live-debug.md; just live-down stops everything.`

The failing check before the change is `just live-check`: the recipe does not exist, so `just` exits 1 with `Justfile does not contain recipe`. There is no Go test, because the recipe is a shell orchestration whose evidence is its own assertions: `requests.jsonl` counts and HTTP status codes from `curl`.

- Lands after: Steps 1 and 2 of this workstream, and the ratchet workstream, Step 3, which edits the `dist` recipe in place.

Anchor: Herbert Simon, Edsger W. Dijkstra.

