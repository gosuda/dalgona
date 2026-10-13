# Slash commands

Built-in commands, in registry order. Plugin commands appear after these with their own names.

| command | hint | summary |
|---|---|---|
| /settings |  | view and change settings |
| /model | [provider/model] | pick the model |
| /tree |  | navigate the session tree |
| /thinking | [level] | set the thinking level |
| /scoped-models |  | choose the models that /model lists |
| /export | [path] | write the session to a .md or .jsonl file |
| /import | <path> | open a session from a .jsonl file |
| /share |  | not in dalgon: use /export |
| /bug |  | not in dalgon: use /export and /session |
| /copy |  | copy the last reply to the clipboard |
| /name | [name] | set or show the session name |
| /session |  | show session details and usage |
| /changelog |  | show what changed in each version |
| /hotkeys |  | show keyboard shortcuts |
| /fork |  | new branch from a previous message |
| /clone |  | copy this session into a new one |
| /trust |  | not in dalgon: plugins load only from the data directory |
| /login | [provider] | sign in to a provider |
| /logout | [provider] | remove stored credentials |
| /new |  | start a new session |
| /compact | [instructions] | summarize older context now |
| /resume | [id or name] | open another session |
| /reload |  | reload plugins |
| /quit |  | quit dalgon |
| /mode | <normal\|eval-first\|eval-only> | set the harness mode |

`/share`, `/bug`, and `/trust` never upload; `/share` writes a file you send yourself. A conflicting command name is an error naming both claimants; the numeric-suffix form `/review:1` is rejected. Completion ranks exact prefixes first, then substrings.
