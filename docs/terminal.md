# The terminal interface and its keys

Inline by default; fullscreen on request. Keys:

| key | action | F1 label |
|---|---|---|
| enter | submit | Send |
| esc | close or interrupt | Close or interrupt |
| up | previous history | Previous history |
| down | next history | Next history |
| F1 | help | Help |
| pgup / pgdn | page through the transcript | Transcript page up / Transcript page down |
| end | jump to the latest transcript entry | Jump to latest |
| ctrl+t | transcript overlay (inline) | Transcript overlay |
| ctrl+shift+f or F3 | search the transcript (fullscreen) | Search transcript |
| shift+left | extend the fullscreen selection left | Extend selection left |
| shift+right | extend the fullscreen selection right | Extend selection right |
| shift+up | extend the fullscreen selection up | Extend selection up |
| shift+down | extend the fullscreen selection down | Extend selection down |

Press F1 to open keyboard help without adding it to the transcript. Use Up and
Down to scroll one row, or PageUp and PageDown to scroll one page. Press Esc or
Ctrl+C to close help. Your draft stays unchanged, and help ignores typing and
paste. Run `/hotkeys` instead to keep the key list in the transcript.

In fullscreen, and in the Ctrl+T overlay, the transcript viewport follows the live edge; paging up detaches it and the hint shows `following stopped · press end to jump to the latest` until End returns to the latest entry.

In fullscreen, hold Shift and use the arrow keys to select transcript text.
Press Ctrl+C to copy the selection without interrupting a running turn.
Typing clears the selection and returns to the composer. In inline mode, use
your terminal's native text selection.

Diagram labels use installed fonts captured when the terminal interface starts.
Restart after installing fonts. If a diagram cannot resolve a font, dal shows
the original source and a failure reason instead of an unlabeled image.

In fullscreen only, Ctrl+Shift+F (F3 on terminals without kitty keyboard) opens a filter row over the viewport top; Enter jumps to the latest match and Esc closes it.

When the layout has room, a busy extension has a row `<extension>: <text>` (or `<extension>: busy` with no text) above the status line in the dim role and in extension-name order. When more extensions are busy than the activity rows allow, the last row reads `(<n> more extensions busy)` when multiple extensions are hidden, or `(1 more extension busy)` when one is hidden. A quiet extension has no row, and the rows follow the terminal's color and `NO_COLOR` settings. F1 opens help; steer types while dal works; follow up waits for the turn; stop ends it at once. Copy uses the terminal clipboard. The first non-empty locale selects ambiguous character width: Japanese, Chinese, and Korean select wide; other locales select narrow. Themes tune contrast; opt-in inline images show diagrams; native diagrams render as text when images are off.
