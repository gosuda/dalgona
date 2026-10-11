---
description: Fires on assistant-protocol meta talk that serves no task.
condition:
  - '(?i)\bas an ai (?:language )?model\b'
  - '(?i)\bi do not have (?:access|the ability) to\b'
scope: text
interruptMode: prose-only
repeatMode: after-gap
repeatGap: 10
---
Answer as the coding agent. Use the tools you have and continue the task.
