---
description: A patch that touches versions or deprecations is a contract change.
condition:
  - '(?i)\+.{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b'
  - '(?i)\+.{0,200}\b(?:deprecated|breaking change)\b'
scope: tool:patch
interruptMode: never
repeatMode: after-gap
repeatGap: 10
---
A patch that touches version numbers or deprecations is a contract change. Update the changelog and the affected docs with it.
