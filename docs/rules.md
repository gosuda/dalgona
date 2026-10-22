# Rules: stream, always, and rulebook

Three kinds of rules exist. Stream rules watch output. Always rules apply every turn. Rulebook files hold project rules. User rule files live under `<data-root>/rules/`. Project rule files live under `<workspace>/.dal/rules/`. Enabled plugins can also supply files from their `rules/` directories. Read a rule at `rule://<name>`.

## Interrupts and retries

Stream rules can interrupt a response and supply a reminder for the retry. `rules.max_retries` caps these interrupts per turn. At the cap, unjudged matches supply reminders without interrupting. Judged matches become report-only: they do not call the judge or add a reminder. A report-only rule never interrupts.

dal writes rule reminders to the session journal before delivery. They enter model context and its token estimate. They remain available when you resume the session. A session keeps its captured rule set. Editing a rule file does not change an already-open session.

## Judged rules

A judged rule has a pattern and a yes-or-no judge question. Session startup checks the selected judge route and its credentials without generating a response or consuming a scripted response. Readiness does not guarantee that a later model request will succeed.

`rules.judge` controls which judged rules enter the session's matching set:

- `off` excludes judged rules.
- `auto` includes them when the shared judge is ready. Otherwise, it excludes them and reports a readiness note.
- `on` requires a ready shared judge. If none is ready, session startup fails.

The shared judge has a separate `gate` setting under `[judge]`. Setting `judge.gate = "off"` skips its readiness check. Setting `rules.judge = "off"` does not disable the shared judge, which other features can use. If either gate is `on`, an unavailable judge prevents startup. dal returns unexpected readiness errors. It does not treat them as missing credentials.

When a judged match can interrupt, dal closes the provider stream before requesting the verdict:

- `true` confirms a violation. dal records the rule fire and retries with its reminder.
- `false` rejects the match. dal reports one informational pass note and retries without a rule fire or reminder.
- A judge error or timeout reports one informational failure note and retries without a rule fire or reminder.

dal latches the rule for that turn before the verdict wait. A retry does not judge the same rule again. Cancellation stops the wait without a late rule fire or retry. The configured retry cap still applies.

See [Settings in dal.toml](config.md) for rule defaults.

## Next

- [Plugins](plugins.md#add-skills-rules-and-prompt-text): ship rule files from a plugin.
- [Settings in dal.toml](config.md): set `rules.judge` and the other rule defaults.
