---
description: Fires on credential-looking literals added by a patch.
condition: '(?i)(?:api[_-]?key|secret|password|token|credential)["'']?\s*[:=]\s*["''][A-Za-z0-9+/_=-]{16,}["'']'
scope: tool:patch
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
That literal looks like a credential. Never write credentials into the repo. Read them from config or the environment at run time.
