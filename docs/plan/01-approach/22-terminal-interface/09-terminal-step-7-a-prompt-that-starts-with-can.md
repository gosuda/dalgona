#### Terminal step 7. A prompt that starts with `/` can be sent

- `slash.go`: add `type command struct{ name, args, summary string }` and `var commands = []command{...}`, holding `model [id]`, `resume [id]`, `new`, `compact`, `login [provider]`, `tree`, `revert`, `fork`, `advisor [status|reset]`, each with a one-line summary.
- This step owns the slash rule and the command list. Step 22 and the web workstream cite `commands` and `parseSlash` and add no copy of either.
- Add `func parseSlash(text string) (name, arg string, kind slashKind)` with kinds `slashCommand`, `slashPrompt` and `slashUnknown`. `//x` gives a prompt with the text `/x`. A first token with a second `/` gives a prompt with the whole text. A first token that matches `commands` gives a command. Anything else is unknown.
- `commands.go` `submitPrompt` (`commands.go:116-134`): use `parseSlash`. `submitPrompt` clears the editor at `commands.go:121` before it dispatches, so an unknown command restores the text to the editor. It sets the notice `unknown command /<name>; start with // to send it as a prompt` (one row, step 2).
- Test (fails before): `TestSlashPrefixedPromptCanBeSent`. `//etc is odd` sends `turn/start` with the input `/etc is odd`. `/etc/hosts is wrong` sends the whole text. `/modle` makes no call, the editor keeps `/modle`, and the notice contains `start with //`. The expected values come from this step's text.
- Spec: `docs/spec/tui.md` keys table, new row `//text`.
- Lands after: Steps 1 and 2.
- Anchor: Claude Shannon.

