# skill-pack: Skills that port and write plugins.

## Ported from
pi `dynamic-resources`

## What it shows
A plugin that publishes only skills. `port-pi-extension` routes the model to `dal://convert-pi` when you name a pi extension. `write-plugin` routes it to `dal://plugins` and `dal://examples` when you ask for a new tool, command, or skill. Each skill body is a short file under `skills/`, read when the plugin loads.

## What differs from pi
Skills are fixed at load. pi's `dynamic-resources` discovers resources while it runs; a plugin cannot.

## Install
Copy to the dal plugins data root and add `skill-pack` to `plugins`.

## Settings
None.
