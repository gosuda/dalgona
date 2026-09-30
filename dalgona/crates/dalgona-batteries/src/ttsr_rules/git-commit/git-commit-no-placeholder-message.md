---
description: Fires on placeholder commit messages.
condition: '(?i)\bgit commit\b.{0,200}-m\s+["''](wip|tmp|temp|fix|update|stuff|changes|misc)["''](\s|$)'
scope: tool:exec
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
Write a commit message that names what changed and why. Never commit with a placeholder message.
