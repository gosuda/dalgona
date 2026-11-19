#### Comment removal step 19. Hashline model checker comments

Anchor: Richard Feynman

- Files: `docs/spec/hashline/verify_model.py`, `docs/spec/hashline/README.md`, `docs/spec/edit-styles.md`.
- Change: first add the two README sentences and change the edit-styles sentence below. Then delete the module docstring at lines 1 to 6, the comments at lines 71, 180, 247, 255 and 266, and the docstrings at lines 89 and 104. The code lines stay as they are.
- Spec text, `docs/spec/hashline/README.md`: as a new paragraph under `## Executed evidence`, after the paragraph that starts `This is not a filesystem implementation`: The model checker MUST NOT stand in for the parser, journal, observation transport, or production patch engine. The model MUST give each line of the base file its own identity, even when its text repeats.
- Spec change, `docs/spec/edit-styles.md`: in `## References and lineage`, replace the sentence `A later undo MUST NOT revive a conflict.` with `A later undo or edit that restores adjacency MUST NOT revive a conflict.`
- Existing homes that need no new text: line 71 is stated by `Do not claim universal occurrence identity` in `docs/spec/hashline/hashline-common-v1.md` (lines 43-44). Line 89 is shown by `map_span` (lines 90-103) and by `hashline-common-v1.md` lines 188-189. Line 104 is stated by `A gap MUST survive only while both original neighbors remain adjacent.` in `edit-styles.md`. Line 180 is stated by the changed edit-styles sentence. Line 247 is shown by the asserts at lines 248-253. Line 255 is shown by the asserts at lines 256-263 and by `Enhanced MUST require every affected source row in the shown ranges.` under `### Hashline Light and Enhanced`. Line 266 is shown by the assert at line 267 and by `hashline-common-v1.md` line 306, `Expand every effective footprint to include all changed raw line cells.`
- Tests that fail before the change: none. The model output must not change.
- Check: `just verify` passes. `grep -n '#' docs/spec/hashline/verify_model.py` returns no line, and `grep -n '"""' docs/spec/hashline/verify_model.py` returns no line. Outside check 3 runs the model.
- Lands after: Comment removal step 18

