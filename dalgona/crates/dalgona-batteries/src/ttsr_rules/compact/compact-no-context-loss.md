---
description: Fires when the model claims it lost earlier context.
condition: '(?i)\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 5
---
State lives in files, not memory. Read the todo list and the files you touched, then continue.
