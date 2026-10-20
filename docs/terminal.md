# dal terminal interface: keyboard shortcuts, inline and fullscreen

The interface is inline by default. It is fullscreen on request. Keys:

| key | action | F1 label |
|---|---|---|
| enter | submit | Send |
| esc | close or interrupt | Close or interrupt |
| up | previous history | Previous history |
| down | next history | Next history |
| ctrl+up | previous prompt | Previous prompt |
| ctrl+down | next prompt | Next prompt |
| ctrl+r | search history | Search history |
| tab / shift+tab | cycle a completion forward or back | Complete |
| ctrl+g | edit the draft in $VISUAL or $EDITOR | External editor |
| ctrl+l | clear the live region | Clear live region |
| ctrl+z | suspend to the shell (not on Windows) | Suspend |
| F1 | help | Help |
| pgup / pgdn | page through the transcript | Transcript page up / Transcript page down |
| end | jump to the latest transcript entry | Jump to latest |
| ctrl+t | transcript overlay (inline) | Transcript overlay |
| ctrl+shift+f or F3 | search the transcript (fullscreen) | Search transcript |
| tab / shift+tab | next or previous action in a dialog | Next action / Previous action |
| shift+left | extend the fullscreen selection left | Extend selection left |
| shift+right | extend the fullscreen selection right | Extend selection right |
| shift+up | extend the fullscreen selection up | Extend selection up |
| shift+down | extend the fullscreen selection down | Extend selection down |

Press F1 to open keyboard help without adding it to the transcript. Use Up and
Down to scroll one row. Use PageUp and PageDown to scroll one page. Press Esc or
Ctrl+C to close help. Your draft stays unchanged. Help ignores typing and
paste. Run `/hotkeys` instead to keep the key list in the transcript.

In fullscreen, and in the Ctrl+T overlay, the transcript viewport follows the live edge. Paging up detaches it. The hint shows `following stopped · press end to jump to the latest` until End returns to the latest entry.

In fullscreen, hold Shift. Use the arrow keys to select transcript text.
Press Ctrl+C to copy the selection without interrupting a running turn.
Typing clears the selection. It returns you to the composer. In inline mode, use
your terminal's native text selection.

Diagram labels use installed fonts captured when the terminal interface starts.
Restart after installing fonts. If a diagram cannot resolve a font, dal shows
the original source and a failure reason instead of an unlabeled image.

In fullscreen only, Ctrl+Shift+F (F3 on terminals without kitty keyboard) opens a filter row over the viewport top. Enter jumps to the latest match. Esc closes it.

When the layout has room, a busy extension has a row `<extension>: <text>` (or `<extension>: busy` with no text) above the status line. The row uses the dim role and extension-name order. When more extensions are busy than the activity rows allow, the last row names the hidden count. It reads `(<n> more extensions busy)` when multiple extensions are hidden. It reads `(1 more extension busy)` when one is hidden. A quiet extension has no row. The rows follow the terminal's color and `NO_COLOR` settings. F1 opens help. Steer types while dal works. Follow up waits for the turn. Stop ends it at once. Copy uses the terminal clipboard. The first non-empty locale selects ambiguous character width. Japanese, Chinese, and Korean select wide. Other locales select narrow. Themes tune contrast. Opt-in inline images show diagrams. Native diagrams render as text when images are off.

## Next

- [Slash commands](commands.md): run `/hotkeys` to keep the key list in the transcript.
- [Command line](cli.md): start fullscreen with `--screen fullscreen`.
- [Settings in dal.toml](config.md): set the `screen` default.
