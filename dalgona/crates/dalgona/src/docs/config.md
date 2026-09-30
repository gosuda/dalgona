# dalgona settings that differ from dalgon

Dalgona reads `$XDG_CONFIG_HOME/dalgona/dal.toml` and keeps data in `$XDG_DATA_HOME/dalgona/`; dal uses `dal/` names.

| key | dalgon default | dalgona default |
|---|---|---|
| mode | `normal` | `normal` |
| model | none | none |
| thinking | `medium` | `medium` |
| approval | `ask` | `ask` |
| screen | `inline` | `inline` |
| tui.diagrams | `false` | `false` |
| theme | `auto` | `auto` |
| sandbox | `false` | `false` |
| images | `false` | `false` |
| motion | `true` | `true` |
| compact_ratio | `0.85` | `0.85` |
| edit_style | `anchor` | `hashline` |
| guard.enabled | `false` | `true` |
| search_symbols | `false` | `true` |
| judge | `auto` | `auto` |
| plugins | `[]` | `[]` |
| aliases | none | none |
| serve.bind | `127.0.0.1` | `127.0.0.1` |
| serve.port | `7437` | `7437` |
| serve.token_file | none | none |
| serve.approval | `ask` | `ask` |
| serve.origins | `[]` | `[]` |
| rules.watch | `true` | `true` |
| rules.interrupt | `always` | `always` |
| rules.repeat | `once` | `once` |
| rules.repeat_gap | `10` | `10` |
| rules.max_retries | `3` | `3` |
| rules.disabled | `[]` | `[]` |
| rules.judge | `auto` | `auto` |
| disabled_batteries | none | `[]` |
| experimental_batteries | none | `[]` |
| agents.enabled | `false` | `true` |
| agents.max_concurrent | `32` | `32` |
| agents.max_depth | `1` | `1` |
| limits.agents | `64` | `64` |
| limits.agents_per_session | `1024` | `1024` |
| limits.agent_depth | `1` | `1` |
| guard.policies.g4_enabled | `true` | `true` |
| plugin.history.enabled | none | `true` |
| plugin.history.share | none | `0.4` |
| plugin.orchestration.loop_guard.enabled | none | `true` |
| plugin.orchestration.sleep.enabled | none | `true` |
| plugin.orchestration.monitor.enabled | none | `true` |
| plugin.orchestration.monitor.coalesce_ms | none | `2000` |
| plugin.orchestration.monitor.rate_limit_ms | none | `5000` |
| plugin.orchestration.monitor.max_lines | none | `50` |
| plugin.orchestration.monitor.max_chars | none | `4096` |
| plugin.orchestration.monitor.wake_budget | none | `5` |
| plugin.orchestration.inflight.enabled | none | `true` |
| plugin.orchestration.goal.enabled | none | `true` |
| plugin.orchestration.arbiter.enabled | none | `true` |
| plugin.orchestration.agents.enabled | none | `true` |
| plugin.orchestration.agents.child_max_steps | none | `50` |
| plugin.orchestration.agents.child_max_minutes | none | `30` |
| plugin.orchestration.agents.max_runs | none | `16` |
| plugin.orchestration.isolation.enabled | none | `true` |
| plugin.plan.enabled | none | `true` |
| plugin.judged.thinking | none | `true` |
| plugin.judged.ranking | none | `true` |
| plugin.judged.ask_anchor | none | `true` |
| plugin.judged.claim_check | none | `true` |
| plugin.judged.dedup | none | `true` |
| plugin.mcp.enabled | none | `true` |
| plugin.review.enabled | none | `true` |
| plugin.review.max_rounds | none | `3` |
| plugin.review.reviewer_model | none | empty: the session model |
| plugin.review.diff_base | none | empty: `HEAD` |
| plugin.web.enabled | none | `true` |
| plugin.web.provider | none | `brave` |
| plugin.web.api_key_env | none | `BRAVE_API_KEY` |
| plugin.web.timeout_secs | none | `30` |
| plugin.web.max_redirects | none | `10` |
| plugin.web.max_body_bytes | none | `2097152` |
| plugin.web.max_markdown_bytes | none | `131072` |
| rule_sets.enabled | none | `["steer", "compact", "stop", "docs", "atlas-v2", "project-workflow", "git-commit", "detectors"]` |

Set `[tui].diagrams` to `true` to render supported fenced diagrams in transcript rows and ask previews. It defaults to `false`.

```toml
search_symbols = true
edit_style = "hashline"
disabled_batteries = []

[tui]
diagrams = false
```
