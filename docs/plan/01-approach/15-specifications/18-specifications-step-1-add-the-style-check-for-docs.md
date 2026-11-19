#### Specifications step 1. Add the style check for `docs/spec`
Anchor: Edsger W. Dijkstra, Edward Tufte

- New file `docs/spec/check_style.py` with exactly this text:

```python
import pathlib
import re
import sys

SENTENCE_LIMIT = 25
STEP_LIMIT = 20
CODE = re.compile(r"`[^`]*`")
LINK_TARGET = re.compile(r"\]\([^)]*\)")
LIST_MARK = re.compile(r"^\s*(?:[-*+]|\d+\.)\s+")
NUMBERED = re.compile(r"^\s*\d+\.\s+")
SENTENCE_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z\[(`C])")


def findings(path):
    fenced = False
    for number, line in enumerate(path.read_text(encoding="utf-8").split("\n"), 1):
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced or not line.strip() or line.lstrip().startswith(("#", "|", ">")):
            continue
        limit = STEP_LIMIT if NUMBERED.match(line) else SENTENCE_LIMIT
        prose = LINK_TARGET.sub("]", CODE.sub("CODE", line))
        prose = LIST_MARK.sub("", prose)
        if ";" in prose:
            yield number, "semicolon outside code"
        if "\u2014" in prose:
            yield number, "em dash"
        if re.search(r"\bshould\b", prose):
            yield number, "lower-case should"
        for sentence in SENTENCE_END.split(prose):
            words = len(sentence.split())
            if words > limit:
                yield number, f"{words} words, limit {limit}"


def main():
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "docs/spec")
    paths = [root] if root.is_file() else sorted(root.rglob("*.md"))
    count = 0
    for path in paths:
        for number, problem in findings(path):
            count += 1
            print(f"{path}:{number}: {problem}")
    print(f"style findings: {count}")
    return 1 if count else 0


if __name__ == "__main__":
    sys.exit(main())
```

- `justfile`: add this recipe after the `hashline-model` recipe (`justfile:29-43`). It follows that recipe's rule for a host without `uv`.

```
spec-style:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v uv >/dev/null; then
        if [ "${CI:-}" = "true" ]; then
            echo "spec-style: uv is required in CI and is not installed" >&2
            exit 1
        fi
        echo "spec-style: skipped, uv is not installed"
        exit 0
    fi
    uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec
```

- This step does not add `spec-style` to the `verify` recipe. At commit `3c4b6f94` the check prints `style findings: 157` for `docs/spec` (77 sentences over the limit and 80 semicolons outside code), so the gate comes last, in Specifications step 40.
- The check counts words, semicolons, em dashes and a lower-case `should`. It does not judge voice, tense or the choice of terms. Those stay rules for the writer (rule 3 of this plan).
- Proof. Write a file with one sentence of 26 words and run `uv run --no-project --python 3.12 docs/spec/check_style.py <file>`. It prints `<file>:1: 26 words, limit 25` and `style findings: 1`, and it exits with status 1. Cut one word. It prints `style findings: 0` and exits with status 0. No Go test is needed, because the check is a script outside the module.
- Specification: `docs/README.md`. Add one sentence under the list of specifications: `` `just spec-style` checks sentence length, semicolons and dashes in `docs/spec`. ``

