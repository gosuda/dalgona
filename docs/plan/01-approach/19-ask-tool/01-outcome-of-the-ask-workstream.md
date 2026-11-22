#### Outcome of the ask workstream

The working model in `dal` and `dalgona` can ask one to four questions and receive a plain-text tool result.
Questions offer two to six options, the automatic Other row, optional multi-select and previews, or a free-text field.
Required model fields are validated before display. Blank questions and unanswerable option values never reach a card.
GUI, TUI, and web preserve accepted text and use the shared 2000-scalar answer limit without silent truncation.
The first valid answer wins. A text-validation warning leaves the original request and editable draft pending for correction.
The original five-minute deadline, interruption, dismissal, last-subscriber loss, and restart rules determine unanswered outcomes.
Subagents and advisors never see the tool. Questions and results remain represented by the existing tool events, without a separate question journal.

