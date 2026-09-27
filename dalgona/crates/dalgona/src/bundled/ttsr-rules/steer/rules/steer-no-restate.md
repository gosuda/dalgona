---
description: Fires when the model restates the request instead of working.
condition: '(?i)\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not restate the request. Start or continue the work.
