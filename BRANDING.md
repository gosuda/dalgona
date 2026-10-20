# BRANDING: dal and dalgona

This file is the naming and identity source of truth.
Product behavior lives in `CONCEPTS.md` and `DESIGN.md`.

## Names

- `dal` (달, "moon"): the minimalistic core, the agent engine and the terminal
  interface. Motif: the moon.
- `dalgona` (달고나, the Korean sugar candy): the opinionated batteries-included
  harness around `dal`.
- `dalcom`: reserved for the desktop sibling, the codex-desktop /
  DeepSeekHarness-Desktop equivalent (github.com/dataelement/dsh-desktop). Not
  part of the dal/dalgona workspace; claims no crate, binary, or env prefix yet.
- Relationship: dal is the moon; dalgona is what you build on it.
  One brand family, two products.

## Spellings

- Library crates: `dal-*` and `dalgona-*`. Binaries: `dalgon` (primary, with
  `dal` and `dl` aliases) and `dalgona` (with `dg`). The product's spoken
  name is `dal`; the historical binary name stays for muscle memory and
  script compatibility.
- Environment and config namespace: `DAL_*`, `dal.toml`, and the per-family
  XDG roots (config default `~/.config/dal/`, data default
  `~/.local/share/dal/`; `dalgona` uses its own family directories);
  harness-level keys use `DALGONA_*` where the two must differ.
- Prose: lowercase `dal` and `dalgona` mid-sentence; capitalize only at
  sentence start. The Hangul spellings (달, 달고나) appear in identity
  contexts (README hero, about text), never as identifiers.
- Public repository: `gosuda/dalgona`, whose root Cargo workspace contains the
  `dal-*` crates, `dalgon`, and `dalgona` packages.

## Voice

- Sentence case; verb-first actions; no exclamation marks; no `oops`.
- Errors name what happened and the fix.
- The moon motif stays subtle: at most one visual mark (a crescent or disc) in
  the hero of user-facing surfaces; no moon puns in error text or status copy.

## Logo

- Use a text mark with a minimal crescent/disc glyph for the user-facing
  identity. Keep moon-first, candy only as the harness's personality; never
  cartoonish.
