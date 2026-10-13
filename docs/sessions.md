# Sessions, history, and compaction

Each session is a journal; a record counts as saved only after it is synced to disk. Sessions live under the data root. `-c` continues, `-r` resumes; names label sessions; `/tree`, `/fork`, `/clone` reshape the tree; `/export` writes a shareable file. Compaction starts by default at 85 percent of the context window, remote first; `/compact <focus>` forces it. Pointers live at `session://<id>?from=<entry>`. `--no-session` runs without saving.

Child sessions also write a `dal-agent` `child_policy` record with their inherited approval mode and tool allowlist; an absent tool list means unrestricted access.
