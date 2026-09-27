---
description: Fires on completion claims that no run in this session backs.
condition: '(?i)\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
A completion claim needs evidence from this session. Run the project gates and quote their output before you claim done.
