# Static approval rules

The rules package MUST classify shell commands before a model decider runs.

`Split` MUST split at shell operators and newlines, ignoring quoted operators. `Check` MUST detect substitution, shell-wrapper, privilege, nested-shell, inline-interpreter, recursive-delete, git-destructive, publish, deploy, and credentials using RE2. `recursive-delete` MUST match an `rm` invocation with a recursive flag (`-r` or `-R` in any combined or separate short flag, or `--recursive`) in any flag position. A match MUST NOT reach an automated decider.

`GrantPrefix` MUST reject compound commands and tokenize POSIX quotes and escapes. It MUST return the first token and the second token when that token does not start with `-`. `MatchesGrant` MUST accept only the same prefix.

Malformed or empty input MUST produce no grant. Unknown command shapes MUST remain subject to normal approval.
