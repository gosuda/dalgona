---
description: Fires when the model announces a workaround or quick fix.
condition: '(?i)\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Name the root cause and fix it there. If a workaround is truly needed, write its reason and its removal condition next to the code.
