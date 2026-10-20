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

`/name`'s confirmation prints a resume token after the name: `Resume it with dalgon -r <token>.` The token is the name itself when it is shell-safe. Shell-safe means letters, digits, `.`, `_`, `-` bare, and spaces and apostrophes in double quotes. Otherwise the token is the session id. Paste it as shown. Anything the shell would expand or split always arrives as the id.

`/share`, `/bug`, and `/trust` never upload. `/share` directs you to `/export`, which writes a file you can send yourself. A conflicting command name is an error `command "<name>" is already registered by plugin "<first>"; plugin "<second>" cannot register it`. The numeric-suffix form `/review:1` is rejected. Completion matches name prefixes and orders them by name.

## Remote sign-in

Esc cancels a remote API-key sign-in only before you submit the key. After submission, wait for the server's result. If it succeeds, use `/logout` to remove the stored credential.

## Skills

A skill is not a slash command. `/` completion never lists `skill:` entries. Run one with `/skill:<name> [text]`. The command sends `text` with it. Or run one with a `$` token in any prompt.

- `/skill <name>` runs the skill when the name is exact. Any other text lists the skills whose name contains it and runs nothing: `/skill find-` prints every candidate and starts no turn.
- In the composer, a token that starts with `$` opens a skills-only completion list. The list has the same match and order as `/`. Tab puts the first match in place of that token.
- A prompt that holds the whole token `$<name>`, `$<plugin>:<name>`, or `/skill:<name>` runs that skill. A prompt may name several. The body of each named skill joins the turn once, after your message, in order of first mention. Any other token stays text.

## Next

- [Sessions, history, and compaction](sessions.md): use `/tree`, `/fork`, `/clone`, `/export`, and `/compact`.
- [Tools, modes, and approval](tools.md): set the mode that `/mode` changes.
- [Changes by version](changelog.md): read what `/changelog` prints.
