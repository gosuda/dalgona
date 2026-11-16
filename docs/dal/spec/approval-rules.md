# Static approval rules

The rules package MUST classify shell commands before a model decider runs.

The `shellargs` package MUST supply `Split`, `Tokens`, `QuoteToken`, `StripWrappers` and `IsWrapper` to the rules package. The rules package MUST NOT declare a second parser.

`Split` MUST split at shell operators and newlines, ignoring quoted and escaped operators. `Check` MUST detect substitution, shell-wrapper, nested-shell, inline-interpreter, recursive-delete, git-destructive, publish, deploy, credentials, and privilege. `Check` MUST test the whole command for substitution first. Then `Check` MUST test the parts from left to right. For each part, `Check` MUST test the other rules in the order of this list. `Check` MUST stop at the first match. Every rule except recursive-delete and shell-wrapper MUST use RE2.

Substitution is `$(`, `<(`, `>(`, a backtick, or a backslash before a line break. A match inside quotes counts. `Check` MUST return the rule name, the text of the matching part, and `true`. The text of a part is its trimmed original text, with its wrappers and assignments. For substitution, the part is the whole command. When nothing matches, `Check` MUST return an empty rule name, an empty part, and `false`.

`recursive-delete` MUST match an `rm` invocation with a recursive flag. An `rm` invocation is a wrapped command whose name is `rm`, so `/bin/rm -rf x` matches. A recursive flag is `-r`, `-R`, or `--recursive`, combined with other short flags or separate, in any flag position before a `--` token. A token after `--` MUST NOT count as a flag. A match MUST NOT reach an automated decider.

## Wrapper stripping

`Check` MUST match each rule except substitution against the wrapped command of a command part. `StripWrappers` MUST remove leading items from the tokens of a part. The remaining tokens are the wrapped command. The name of the wrapped command is its first token without a directory prefix.

Every RE2 rule except substitution MUST match the normal form of the wrapped command. The normal form is the name of the wrapped command, then each later token in the form that `QuoteToken` returns, joined by single spaces. The credentials rule MUST also match the original text of the part, including removed options and values.

The leading items are `NAME=value` assignments, shell control keywords, and the wrappers in the table below. The shell control keywords are `if`, `then`, `elif`, `else`, `fi`, `while`, `until`, `do`, `done`, `for`, `in`, `case`, `esac`, and `!`.

|Wrapper|Options removed|
|---|---|
|`env`|`-i`, `-0`, `-u NAME`, `-C DIR`, `--ignore-environment`, `--null`, `--unset=NAME`, `--chdir=DIR`, then each `NAME=value`|
|`command`, `time`|`-p`|
|`exec`|`-c`, `-l`, `-a NAME`|
|`nice`|`-n N`, `-<digits>`, `--adjustment=N`|
|`timeout`|`-v`, `-s SIGNAL`, `-k DURATION`, `--foreground`, `--preserve-status`, `--verbose`, `--signal=SIGNAL`, `--kill-after=DURATION`, then one duration|
|`sudo`, `doas`|`-E`, `-n`, `-u USER`|
|`builtin`, `nohup`, `watch`, `npx`, `bunx`, `pnpx`|None|

Each wrapper accepts `--` to end its options. A wrapper name MAY carry a directory prefix. A `timeout` duration is digits with an optional fraction and an optional `s`, `m`, `h` or `d` suffix. A short option group MAY combine flags, as in `-nu root`. A short option takes its value from the next token or from the rest of its own token, as in `-u root` and `-uroot`. A long option takes its value after `=`, so `--signal TERM` is outside the table. An option without its value is outside the table.

A wrapper with an option form outside the table MUST stay in the command with all later tokens. A `timeout` without a valid duration is such a wrapper. `Check` MUST report shell-wrapper when a wrapper stays at the front of the wrapped command.

`Check` MUST report a malformed part as shell-wrapper. `Check` MUST also report shell-wrapper when the name of the wrapped command contains `$` or `=`, starts with `-` or a digit, or is empty. An empty quoted word is a token, so it gives an empty name.

A part that holds only assignments and wrappers has no wrapped command. For such a part `Check` MUST report no rule. The credentials rule MUST NOT read the original text of such a part.

`Check` MUST report privilege when `StripWrappers` removed `sudo` or `doas` and no other rule matches the wrapped command. Privilege is the last rule, so a match of another rule wins. For example, `sudo rm -rf /` reports recursive-delete and `sudo ls` reports privilege. The privilege rule MUST also match a wrapped command that starts with `su`, `sudo` or `doas`.

`CheckRemoteValue` MUST apply the same stripping and every rule except substitution and shell-wrapper. Substitution and shell-wrapper are the shell-syntax rules. The other rules are the semantic rules. For a malformed part, `CheckRemoteValue` strips nothing and matches the RE2 rules against the original text of the part. Recursive-delete never matches a malformed part. Wrapper stripping serves deny matching only. It MUST NOT make a wrapper or its command safe for a read-only fast path.

`GrantPrefix` MUST reject compound commands and tokenize POSIX quotes and escapes. It MUST return the first token. When the command has a second token, it MUST also return that token, including a token that starts with `-`. It MUST NOT strip wrappers or assignments. It MUST write each token with `QuoteToken` and join the tokens with one space. `MatchesGrant` MUST accept only the same prefix. It MUST compare tokens, not text. It MUST reject a compound command, a malformed command, a malformed grant, an empty grant, and a command with fewer tokens than the grant. Both functions count an empty quoted word as a token.

Malformed or empty input MUST produce no thread grant. Unknown command shapes MUST remain subject to normal approval.

An unterminated quote or a trailing backslash is malformed.
