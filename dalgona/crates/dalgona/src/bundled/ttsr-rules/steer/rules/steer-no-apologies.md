---
description: Keeps the reply moving when the model apologizes or agrees.
condition: '(?i)\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:''re| are) (?:absolutely )?right)\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Do not apologize and do not confirm the user was right. State the next concrete step and take it.
