#### Specifications step 35. Correct `docs/spec/web.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/web.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 103 | - Item statuses read `Running`, `Done`, `Failed`, or `Declined`. | - Item statuses read `Running`, `Done`, `Failed`, `Declined`, or `Waiting for approval`. Other statuses show their raw value. | web/src/lib/labels.ts:50-54 `inProgress: "Running"`, `awaitingApproval: "Waiting for approval"`, `completed: "Done"`, `failed: "Failed"`, `declined: "Declined"`; labels.ts:57-59 `export function itemStatusLabel` returning `itemStatusLabels[status] ?? status`. |
| 108 | - The streaming cursor and "Reasoning in progress" show only while the turn runs. | - The streaming cursor shows only while the turn runs. The reasoning label reads `Reasoning in progress` while the turn runs and `Reasoning` otherwise. | web/src/lib/components/Transcript.svelte:127 `{#if !item.complete && running}<span class="streaming" aria-hidden="true">▋</span>{/if}`; Transcript.svelte:131 `<summary>Reasoning{#if !item.complete && running} in progress{/if}</summary>`; Transcript.svelte:41 ` |
| 120 | - With no connection the thread list says `Threads unavailable` and does not claim that no threads exist. A thread without a name shows its preview once. | - With no connection the thread list says `Threads unavailable` and does not claim that no threads exist. A thread without a title shows its preview. A thread with neither shows `Untitled thread`. | web/src/lib/labels.ts:46 `return thread.title \|\| thread.preview \|\| "Untitled thread";` |
| 124 | - In the approval dialog, Decline uses the quiet style. The danger hue marks only Interrupt. | - In the approval dialog, Decline and Cancel use the quiet style. The composer's `Interrupt turn` button has the danger style. | web/src/lib/components/ApprovalDialog.svelte:71-72 `<button class="quiet" onclick={() => decide("decline")}>Decline action</button>` and `<button class="quiet" onclick={() => decide("cancel")}>Cancel</button>`; web/src/lib/components/Composer.svelte:117 `<butt |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 28 | A configured token with any other character MUST fail startup with `web: [web] token must contain only [A-Za-z0-9_-]; '+' and '&' cannot survive the URL fragment`. | internal/webui/webui.go:110-112 `if cfg.Token != "" {` then `if !tokenIsFragmentSafe(cfg.Token) {` returning `fmt.Errorf("web: [web] token must contain only [A-Za-z0-9_-]; '+' and '&' cannot survive the URL fragment")`;  |
| 45 | If the listener cannot bind, startup MUST fail with `web: listen on <addr>: <error>`. | internal/webui/webui.go:163-167 `func listen(ctx context.Context, addr string, tlsConfig *tls.Config)` returning `fmt.Errorf("web: listen on %s: %w", addr, err)`. |
| 52 | A dial failure MUST return the body `daemon unavailable`. | internal/webui/relay.go:35-37 `s.logf("web: dial daemon: %v\n", err)` then `http.Error(w, "daemon unavailable", http.StatusBadGateway)`. |
| 59 | A browser message larger than that limit MUST close the websocket. | internal/webui/relay.go:45 `browser.SetReadLimit(maxMessageBytes)` with relay.go:17 `maxMessageBytes = 16<<20 - 1`; relay.go:55-66 `func (s *server) relay` closes the browser with the first error's status. The status for |

Structure:

- The web workstream, Step 37 proves this sentence with a layout test.
- Depends on other plan steps: the web workstream, Step 4 (page state into a session) may change web.md:122 (reconnect and follow-up discard); steps 10 and 16 may change the labels at web.md:103-108; step 21 (approval card) may change web.md:124.
- Verified: when the fragment has no token and `accessToken` is empty, the page sets `connection` to `disconnected` and shows the alert banner `The URL has no access token. Open the full URL printed by dalgona web.` `tokenFromHash` returns an empty string (web/src/lib/protocol.ts:71-75). Sources: web/src/routes/+page.svelte:134-137, 141-146, 604-606.

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 71 | The HTTP server MUST set a 10 second header read timeout. The HTTP server MUST set a 30 second read timeout. The HTTP server MUST set a 60 second write timeout. The HTTP server MUST set a 120 second idle timeout. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 1 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/web.md` prints `style findings: 0`, and `just verify` passes.

