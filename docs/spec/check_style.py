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
