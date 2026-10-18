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

`/name`'s confirmation prints a resume token after the name: `Resume it with dalgon -r <token>.` The token is the name itself when it is shell-safe (letters, digits, `.`, `_`, `-` bare; spaces and apostrophes in double quotes), otherwise the session id. Paste it as shown; anything the shell would expand or split always arrives as the id.

`/share`, `/bug`, and `/trust` never upload. `/share` directs you to `/export`, which writes a file you can send yourself. A conflicting command name is an error `command "<name>" is already registered by plugin "<first>"; plugin "<second>" cannot register it`; the numeric-suffix form `/review:1` is rejected. Completion matches name prefixes and orders them by name.

## Remote sign-in

Esc cancels a remote API-key sign-in only before you submit the key. After submission, wait for the server’s result. If it succeeds, use `/logout` to remove the stored credential.

## Skills

A skill is not a slash command, and `/` completion never lists `skill:` entries. Run one with `/skill:<name> [text]`, which sends `text` with it, or with a `$` token in any prompt.

- `/skill <name>` runs the skill when the name is exact. Any other text lists the skills whose name contains it and runs nothing: `/skill find-` prints every candidate and starts no turn.
- In the composer, a token that starts with `$` opens a skills-only completion list with the same match and order as `/`. Tab puts the first match in place of that token.
- A prompt that holds the whole token `$<name>`, `$<plugin>:<name>`, or `/skill:<name>` runs that skill, and a prompt may name several. The body of each named skill joins the turn once, after your message, in order of first mention. Any other token stays text.
