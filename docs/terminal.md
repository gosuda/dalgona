# The terminal interface and its keys

Inline by default; fullscreen on request. Keys:

| key | action | F1 label |
|---|---|---|
| enter | submit | Submit |
| esc | cancel | Cancel |
| up | history | History |
| F1 | help | Help |

When the layout has room, a busy extension has a row `<extension>: <text>` (or `<extension>: busy` with no text) above the status line in the dim role and in extension-name order. When more extensions are busy than the activity rows allow, the last row reads `(<n> more extensions busy)` when multiple extensions are hidden, or `(1 more extension busy)` when one is hidden. A quiet extension has no row, and the rows follow the terminal's color and `NO_COLOR` settings. F1 opens help; steer types while dal works; follow up waits for the turn; stop ends it at once. Copy uses the terminal clipboard. The first non-empty locale selects ambiguous character width: Japanese, Chinese, and Korean select wide; other locales select narrow. Themes tune contrast; opt-in inline images show diagrams; native diagrams render as text when images are off.
