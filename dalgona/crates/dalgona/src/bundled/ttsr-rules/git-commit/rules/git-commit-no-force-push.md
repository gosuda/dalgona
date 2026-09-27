---
description: Never force-push; recover by fetch and rebase instead.
condition:
  - '(?i)\bgit push\b.{0,200}\s--force(\s|$)'
  - '(?i)\bgit push\b.{0,200}\s-f(\s|$)'
scope: tool:exec
interruptMode: tool-only
repeatMode: after-gap
repeatGap: 1
---
Never force-push. If the push is rejected, fetch and rebase onto the new tip. Use --force-with-lease only when the user asked for it.
