---
description: Fires when the model cites the conversation instead of a file.
condition: '(?i)\bas (?:mentioned|said|discussed|described) earlier\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not point to earlier conversation. Cite the file and line that hold the fact.
