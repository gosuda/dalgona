#### Web step 34. Shell mode

- Composer. When `parseComposerText` returns `shell`, the label reads `Shell command` and the help line reads `Enter runs the command in ${cwd}. It is not sent to the agent.`
- Session. `runShell(command)` calls `command/exec {command: argv, cwd}`. `argv` is the rule of the terminal workstream, Step 30 (cross-phase decision 14). `Session` reads `platformFamily` from the initialize response; `parseInitializeResponse` (`validation.ts:521-524`) adds it to `InitializeInfo`.
- Result. The command commits one row (not journaled) and adds the chip `[Shell #n exit N, L lines]` for the next prompt (cross-phase decision 14). The chip is an attachment (step 35); its text joins the next `turn/start` input as one more text block.
- Parser. `validation.ts` adds `parseCommandExecResponse` and a conformance row (`CommandExecResponse.json`).
- Test. `session.test.ts`: `shell mode calls command/exec with the platform argv and shows the result`. Fails before: `!` text is sent to the agent.
- Spec (`docs/spec/web.md`). `### Composer`: shell mode.
- Lands after: Steps 28 and 35 (`parseComposerText` and the chip component).

Anchor: John Carmack.

