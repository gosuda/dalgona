# dalgona settings that differ from dal

| key | dal default | dalgona default |
|---|---|---|
| `search_symbols` | `false` | `true` |
| `[tui].diagrams` | `false` | `false` |
| `edit_style` | `anchor` | `hashline` |
| `[guard].enabled` | `false` | `true` |
| `disabled_batteries` | none | `[]` |
| `experimental_batteries` | none | `[]` |
| `rule_sets.enabled` | none | all shipped sets |
| `[plugin.history].share` | none | `0.4` |
| `[plugin.plan].enabled` | none | `true` |
| `[plugin.procs].enabled` | none | `true` |
| `[plugin.web].provider` | none | `brave` |
| `[plugin.web].timeout_secs` | none | `30` |

Set `[tui].diagrams` to `true` to render supported fenced diagrams in transcript rows and ask previews. It defaults to `false`.

Battery tables are decoded strictly. Unknown keys, invalid types, and values outside documented ranges fail during product construction, with one exception: an invalid `[plugin.history]` section warns and disables history at startup so the session still opens, while `/settings` strictly refuses any invalid candidate before saving. `disabled_batteries` is the product-level switch for every battery; supported per-battery `enabled` keys are documented with their battery.

## dalgona and dal

Dalgona does not run a second agent engine. The dal platform is the single underlying agent engine, providing the turn loop, session runtime, tool protocol, and extension boundaries. Dalgona is the compiled batteries product built on dal through dal's public extension interface.

Dalgona configures opinionated defaults for the dal platform (`search_symbols = true`, `edit_style = "hashline"`, `guard.enabled = true`), compiles twelve bundled extensions, maintains its own isolated configuration and data directories (`~/.config/dalgona/` and `~/.local/share/dalgona/` on POSIX defaults), and supplies a product settings catalog to the interactive `/settings` picker.

## Interactive /settings picker

The `/settings` picker in interactive sessions displays allowlisted, non-secret product settings alongside client live controls (model selection, thinking level, and diagram rendering).

### Allowlisted settings rows

Only documented, non-secret settings exposed by dalgona's strictly decoded battery sections appear in the picker. Sensitive credential pointers (`[plugin.web] api_key_env`), user workflows (`[plugin.orchestration.workflows]`), `experimental_batteries`, and unmanaged plugin tables are excluded.

| Group | Key | Control | Description |
|---|---|---|---|
| `batteries` | `disabled_batteries` | subset | List of disabled bundled batteries |
| `ttsr-rules` | `rule_sets.enabled` | subset | Enabled TTSR rule sets |
| `search` | `search_symbols` | toggle | Code symbol indexing and search |
| `guard` | `guard.enabled` | toggle | Complexity and boundary guard |
| `history` | `plugin.history.enabled` | toggle | Visual history compaction |
| `history` | `plugin.history.share` | number | History image budget share (0.1 to 0.7, listed only when history is enabled) |
| `judged` | `plugin.judged.thinking` | toggle | Automatic thinking level selection |
| `judged` | `plugin.judged.ranking` | toggle | Search candidate reranking |
| `judged` | `plugin.judged.ask_anchor` | toggle | Anchored ask classification |
| `judged` | `plugin.judged.claim_check` | toggle | Assistant claim verification |
| `judged` | `plugin.judged.dedup` | toggle | Context reminder deduplication |
| `mcp` | `plugin.mcp.enabled` | toggle | Model Context Protocol client |
| `orchestration` | `plugin.orchestration.loop_guard.enabled` | toggle | Agent turn loop guard |
| `orchestration` | `plugin.orchestration.sleep.enabled` | toggle | Sleep command rewrite |
| `orchestration` | `plugin.orchestration.monitor.enabled` | toggle | Background job monitoring |
| `orchestration` | `plugin.orchestration.monitor.coalesce_ms` | integer | Output batching window |
| `orchestration` | `plugin.orchestration.monitor.rate_limit_ms` | integer | Delivery throttle interval |
| `orchestration` | `plugin.orchestration.monitor.max_lines` | integer | Maximum lines per monitor update |
| `orchestration` | `plugin.orchestration.monitor.max_chars` | integer | Maximum characters per monitor update |
| `orchestration` | `plugin.orchestration.monitor.wake_budget` | integer | Monitor-only wake limit (1 to 100) |
| `orchestration` | `plugin.orchestration.inflight.enabled` | toggle | Inflight status reporting |
| `orchestration` | `plugin.orchestration.goal.enabled` | toggle | Persistent goal tracking |
| `orchestration` | `plugin.orchestration.arbiter.enabled` | toggle | Turn arbitration |
| `orchestration` | `plugin.orchestration.agents.enabled` | toggle | Child agent orchestration |
| `orchestration` | `plugin.orchestration.agents.child_max_steps` | integer | Step limit for child sessions |
| `orchestration` | `plugin.orchestration.agents.child_max_minutes` | integer | Clock limit for child sessions |
| `orchestration` | `plugin.orchestration.agents.max_runs` | integer | Maximum parallel runs |
| `orchestration` | `plugin.orchestration.isolation.enabled` | toggle | Worktree isolation for workflow runs |
| `work` | `plugin.plan.enabled` | toggle | Work planning tools (`/plan`, `/todos`) |
| `procs` | `plugin.procs.enabled` | toggle | External process management tool |
| `review` | `plugin.review.enabled` | toggle | Code review tool (`/review`) |
| `review` | `plugin.review.max_rounds` | integer | Maximum review rounds per session |
| `review` | `plugin.review.reviewer_model` | text | Model route for code review |
| `review` | `plugin.review.diff_base` | text | Target git revision for review diffs |
| `web` | `plugin.web.enabled` | toggle | Web search and fetch tools |
| `web` | `plugin.web.provider` | choice | Search provider (`brave`, `tavily`) |
| `web` | `plugin.web.search_depth` | choice | Tavily search depth (`basic`, `advanced`) |
| `web` | `plugin.web.timeout_secs` | integer | Network request timeout in seconds |
| `web` | `plugin.web.max_redirects` | integer | Maximum HTTP redirect hops |
| `web` | `plugin.web.max_body_bytes` | integer | Maximum raw response bytes |
| `web` | `plugin.web.max_markdown_bytes` | integer | Maximum converted markdown bytes |

### Running versus saved values and restart behavior

Product settings are startup-only. When you confirm a valid value in the picker, the change is written immediately to `dal.toml`. However, the running process does not rebuild its running extensions mid-session.

When the saved value differs from the value the host started with, the picker shows both values and a restart notice. The saved file is read on the next start. Command-line flags still take precedence over the file.

Battery changes require a restart. Because batteries are compiled extensions registered during host initialization, enabling or disabling a battery (via `disabled_batteries` or `[plugin.<name>].enabled`) takes effect only when dalgona restarts.

In contrast, live controls take effect immediately in the active session:

- Diagram rendering (`[tui].diagrams`) can be toggled live in-session, with an explicit picker action to save the preference to `dal.toml`.
- The model row opens the model picker, updating the active model for subsequent turns.
- The thinking row (`thinking · <current level>`) opens the thinking level picker populated with the host's supported levels for the current model. Choosing a level submits `/thinking <level>`, letting the host validate the selection before updating session state and default configuration.

### Atomic persistence and comment preservation

Settings edits are applied directly to the user's `dal.toml`. Existing comments, whitespace formatting, inline annotations, and unrelated configuration tables remain intact.

Before any write, the candidate file is strictly validated through dal configuration layers and dalgona's section decoders. If validation fails (such as an out-of-range integer or an unrecognized provider), the edit is refused, the picker displays the error, and the file on disk remains byte-identical.

Accepted edits are written atomically with owner-only permissions (`0600`), preventing partial writes.

### Remote-host connections

When running dalgona over a remote connection (`--connect <addr>`), product settings belong to the remote host process and its filesystem.

The remote session lists no product rows in `/settings` and rejects product edits. Only local client preferences (such as diagram rendering) are saved locally. To modify product settings for a remote session, edit `dal.toml` directly on the host machine and restart the host process.
