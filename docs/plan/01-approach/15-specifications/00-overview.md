### Workstream: Specifications

This workstream corrects the files under `docs/spec` and `docs/decisions` against the code, and rewrites `docs/spec` in ASD-STE100 (rule 3 of this plan). One step changes one file in one commit. Each step lists the statements to replace, the statements to add, the terms to unify and the long sentences to rewrite, with the evidence for each. Where the text is right and the code is wrong, a spec-fix step changes the code and keeps the text.

The audit read every file under `docs/` against the code. It found nothing to change in these files, so they have no step: `docs/decisions/advisor.md`, `docs/decisions/automated-approval.md`, `docs/decisions/journal-authority-and-mirror.md` and `docs/decisions/web-toolchain.md`. Three more decision files get one step each. Spec-fix step 15 makes `docs/decisions/starlark-replay.md` true. Spec-fix step 16 adds the test that `docs/decisions/sandbox-services.md` requires. Specifications step 39a corrects `docs/decisions/codex-protocol-as-core.md`. Pins step 5 and web Step 1 change `docs/decisions/dependencies.md` and `docs/decisions/web-toolchain.md` for the pins. The tests-and-docs workstream changes `docs/README.md`. `docs/spec/hashline/verify_model.py` changes only in Spec-fix step 11, and `docs/spec/hashline/verification.json` does not change.

A step of this workstream changes text only. It changes no behaviour. The order of steps 2 to 39a is free, because each step owns one file. The spec-fix steps land first, so each sentence that a later step writes is true when it lands.

