# hello: The smallest dal plugin.

## Ported from
pi `hello`

## What it shows
One tool, one prompt section, and one skill in a single `plugin` value. The `hello` tool takes no arguments and returns a greeting. The prompt section tells the model to greet the user warmly. The `hello` skill points at `skills/hello/SKILL.md`.

## What differs from pi
Nothing to translate: pi's `hello` registers one tool, and so does this plugin. The prompt section and the skill are additions that show the other two kinds of registration.

## Install
Copy to the dal plugins data root and add `hello` to `plugins`.

## Settings
None.
