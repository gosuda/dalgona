#### Web step 28. Panels, command palette and the key list

- `web/src/lib/components/panels/Panel.svelte` is created in Step 18, which owns its props and behaviour. This step wires `Session.panel` into it.
- `web/src/lib/commands.ts`
  - `export type PanelKind = "model" | "settings" | "login" | "resume" | "new" | "compact" | "fork" | "tree" | "rewind" | "advisor" | "review" | "keys"`
  - `export const slashCommands: readonly { name: string; summary: string; panel: PanelKind }[]`. The names and their order are the terminal workstream's (Steps 7 and 22). The summaries and panel kinds are the browser's, in this order:
    - `/model` → `Choose a model or preset`
    - `/settings` → `Change theme, verbosity, approval, status items, edit review, and notifications`
    - `/login` → `Log in to a provider`
    - `/resume` → `Open an earlier thread`
    - `/new` → `Start a new thread`
    - `/compact` → `Compact this thread`
    - `/fork` → `Fork this thread at a turn`
    - `/tree` → `Move to another branch of this thread`
    - `/revert` → `Rewind to before a turn`
    - `/advisor` → `Show or change the advisor`
    - `/review` → `Review changes`
    - `/help` → `Show keys`
  - `parseComposerText(text): { kind: "message"; text: string } | { kind: "command"; query: string } | { kind: "shell"; command: string }`:
    - The slash rule (a literal `//`, a command query) is the terminal workstream's, Steps 7 and 22.
    - A `!` typed first in an empty composer gives shell mode (step 34).
- New `web/src/lib/components/CompletionList.svelte`: a `role="listbox"` driven through `aria-activedescendant` from the textarea (`role="combobox"`, `aria-expanded`, `aria-controls`).
- New `CommandPalette.svelte`. Renders through `CompletionList`. Matches by prefix. ArrowUp and ArrowDown move, Enter or Tab opens the panel, Esc closes.
  - No match: `No command named ${query}. Start with // to send a message that begins with /.`
- New `panels/KeyListPanel.svelte` opens with `?` when focus is not editable, and with `/help`. It lists every key from the composer, transcript, request cards and panels.
- Session. `panel: PanelKind | null` and `openPanel(kind)`.
- Tests.
  - `commands.test.ts`: `// sends a literal slash`, `/mod is a command query`, `/etc/hosts with a space is a message`.
  - `CommandPalette.dom.test.ts`: `arrow keys move the active option and Enter opens its panel`.
  - `KeyListPanel.dom.test.ts`: `? opens the key list and Esc returns focus`.
  - Fails before: a leading `/` cannot be sent as text and no palette exists.
- Spec (`docs/spec/web.md`). New `### Commands and panels`: the table, the literal-slash rule (cited from the terminal workstream, Step 7), and the panel wiring. Step 18 owns the panel behaviour.
- Lands after: Steps 18 and 27.

Anchor: Otl Aicher.

