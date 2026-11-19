#### Checks of the web workstream

- Per step. `just verify`, which includes `web-check`: `pnpm -C web exec biome ci`, `pnpm -C web exec vitest run` (both projects) and `pnpm -C web exec svelte-check`.
- Focused runs:
  - `pnpm -C web exec vitest run --project unit src/lib/schema.test.ts`
  - `pnpm -C web exec vitest run --project dom src/lib/components/ApprovalCard.dom.test.ts`
- Build and embed. `just web && go test ./internal/webui/...`.
- Live end to end. Uses the live-debug workstream, assuming the interface in the section "Prerequisites of the web workstream":
  1. `just web`, then `just live-up`, which builds `dalgona`, `dal` and `llmstub` into `/tmp/dg/bin` and starts the daemon and `dalgona web`.
  2. `/tmp/dg/web.url` holds the URL that `live-up` printed. The scripted turns `show markdown` (`t-md`, a Markdown answer with a Go fence), `edit notes` (`t-patch`, an `apply_patch` call under approval mode `ask`) and `ask me` (`t-ask`, an `ask` call with two questions) provide that content.
  3. In the browser session of the live-debug workstream (Step 4), at 1280 px in `day`, run the rows below in order. Each bullet names its row, or is a manual step with no row.
     - W04: type `read a.txt` into `#prompt` and send it.
     - W06: the `Working` tag shows during the turn.
     - W03: the answer renders its code block.
     - W10: the file-change approval shows the diff with its counts, then `Accept`.
     - W13: the question card; choose the first option and submit with the card's submit control.
     - Manual step, no row: after the submit, let the turn finish. Observe that the state tag in the thread header reads `Done` and that `document.title` does not contain `Working`.
     - Every row and the manual step above pass.
  4. Layout and accessibility checks, each as the browser-session row or manual step named below:
     - the layout matrix cell at 320 px. Manual step, no row: create a thread with a 300-character unbroken title and a 200-character path; observe that `document.documentElement.scrollWidth <= innerWidth`.
     - W09 at 320 px. Manual step, no row: with three long diffs in the approval, scroll the details to the end and observe that the Accept button lies inside the viewport.
     - W19 at 768 px. Manual step, no row: with 40 threads, observe that `Start thread` stays in view after the thread list scrolls to its end, and that the list is no taller than 18rem.
     - W05 at 320 px.
     - Manual step, no row: at 375 px, observe that every `input`, `textarea` and `select` has a computed font size of at least 16 px.
     - Manual step, no row: at 375 px, send `long output`, scroll the transcript to its top, and observe that the `Send` button lies inside the viewport.
     - W27, and the layout matrix font probe. The icon subset face has no row.
     - no row. The browser session cannot emulate forced colours (see W24).
     - W24.
     - W25, during a Working turn.
     - the layout matrix probe for buttons and fields at 36 px. Manual step, no row: observe that every visible `a`, `summary` and `[role=option]` is at least 24 px square, and that under a coarse pointer each button and select is at least `--control-height-coarse` tall.
  5. Theme pass: W26, the 3x4 matrix in the four concrete themes. Repeat the rows of item 3 once in `night`.
- Biome scope. `web/biome.json` `files.includes` adds `"checks/**"`.

