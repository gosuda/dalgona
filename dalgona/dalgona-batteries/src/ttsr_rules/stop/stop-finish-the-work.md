---
description: Fires when the model defers reachable work.
condition:
  - '(?i)\bi(?:''ll| will) leave\b'
  - '(?i)\bthe rest (?:is|should be) straightforward\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Finish the work you described. Defer only what the user scoped out, and name it in the todo list.
