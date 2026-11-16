import pathlib
import re
import sys

SENTENCE_LIMIT = 25
STEP_LIMIT = 20
SPAN = re.compile(r"`[^`]*`|\]\([^)]*\)")
LIST_MARK = re.compile(r"^\s*(?:[-*+]|\d+\.)\s+")
NUMBERED = re.compile(r"^\s*\d+\.\s+")
SENTENCE_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z\[(`C])")
SEPARATOR = re.compile(r"^\s*[-=*_]{3,}\s*$")
RULE_CELL = re.compile(r"^:?-+:?$")


def scrub(text):
    pieces = []
    spans = []
    last = 0
    for match in SPAN.finditer(text):
        keep = "CODE" if match.group().startswith("`") else "]"
        pieces.append(text[last : match.start()])
        pieces.append(keep)
        spans.append((match.start(), match.end(), len(match.group()) - len(keep)))
        last = match.end()
    pieces.append(text[last:])
    return "".join(pieces), spans


def shifted(spans, index):
    removed = 0
    for start, end, shrink in spans:
        if end <= index:
            removed += shrink
        elif start < index:
            return start - removed
        else:
            break
    return index - removed


def split_cells(body):
    cells = []
    cell = ""
    in_code = False
    for char in body:
        if char == "`":
            in_code = not in_code
        if char == "|" and not in_code:
            cells.append(cell)
            cell = ""
        else:
            cell += char
    cells.append(cell)
    return cells


def units_of(lines):
    fenced = False
    result = []
    current = []

    def flush():
        if current:
            result.append(list(current))
            current.clear()

    for number, line in lines:
        start = line.lstrip()
        if start.startswith("```"):
            fenced = not fenced
            flush()
            continue
        if fenced:
            continue
        if not line.strip() or start.startswith(("#", ">")) or SEPARATOR.match(line):
            flush()
            continue
        if start.startswith("|"):
            flush()
            result.append([(number, line)])
            continue
        if LIST_MARK.match(line):
            flush()
        current.append((number, line))
    flush()
    return result


def line_at(offsets, index):
    number = offsets[0][0]
    for candidate, begin, end in offsets:
        if begin <= index <= end:
            number = candidate
    return number


def text_findings(limit, fragments):
    if not fragments:
        return
    raw_offsets = []
    position = 0
    for number, fragment in fragments:
        raw_offsets.append((number, position, position + len(fragment)))
        position += len(fragment) + 1
    text, spans = scrub(" ".join(fragment for _, fragment in fragments))
    offsets = [(number, shifted(spans, begin), shifted(spans, end)) for number, begin, end in raw_offsets]
    for problem, needle in (("semicolon outside code", ";"), ("em dash", "\u2014")):
        if needle in text:
            yield line_at(offsets, text.index(needle)), problem
    match = re.search(r"\bshould\b", text)
    if match:
        yield line_at(offsets, match.start()), "lower-case should"
    pieces = []
    start = 0
    for boundary in SENTENCE_END.finditer(text):
        pieces.append((start, boundary.start()))
        start = boundary.end()
    pieces.append((start, len(text)))
    for begin, end in pieces:
        words = len(text[begin:end].split())
        if words > limit:
            yield line_at(offsets, begin), f"{words} words, limit {limit}"


def row_findings(number, line):
    body = line.strip().strip("|")
    for cell in split_cells(body):
        cell = cell.strip()
        if not cell or RULE_CELL.match(cell):
            continue
        yield from text_findings(SENTENCE_LIMIT, [(number, cell)])


def unit_findings(unit):
    limit = STEP_LIMIT if NUMBERED.match(unit[0][1]) else SENTENCE_LIMIT
    fragments = [(unit[0][0], LIST_MARK.sub("", unit[0][1]))]
    fragments.extend(unit[1:])
    yield from text_findings(limit, fragments)


def findings(path):
    lines = enumerate(path.read_text(encoding="utf-8").split("\n"), 1)
    for unit in units_of(lines):
        first_number, first_line = unit[0]
        if first_line.lstrip().startswith("|"):
            yield from row_findings(first_number, first_line)
        else:
            yield from unit_findings(unit)


def main():
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "docs/spec")
    if not root.exists():
        print(f"style: target not found: {root}", file=sys.stderr)
        return 2
    paths = [root] if root.is_file() else sorted(root.rglob("*.md"))
    if not paths:
        print(f"style: no specification files: {root}", file=sys.stderr)
        return 2
    count = 0
    for path in paths:
        for number, problem in findings(path):
            count += 1
            print(f"{path}:{number}: {problem}")
    print(f"style findings: {count}")
    return 1 if count else 0


if __name__ == "__main__":
    sys.exit(main())
