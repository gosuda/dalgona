import os
import pathlib
import re
import sys
from collections.abc import Iterator

EXCLUDED = {
    ".git", ".references", "node_modules", ".svelte-kit", ".outline",
    "dist", ".gitkeep",
}

PRESERVED = pathlib.PurePosixPath("docs/plan")

LINK_RE = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
REF_RE = re.compile(r"(?m)^\[[^\]]+\]:\s*(\S+)")


def heading_id(text: str, seen: dict[str, int]) -> str:
    base = re.sub(r"[^\w\s-]", "", text.lower()).strip().replace(" ", "-")
    base = re.sub(r"-+", "-", base)
    n = seen.get(base, 0)
    seen[base] = n + 1
    if n == 0:
        return base
    return f"{base}-{n}"


def heading_ids(path: pathlib.Path) -> set[str]:
    ids = set()
    seen = {}
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return ids
    for line in text.splitlines():
        m = re.match(r"#{1,6}\s+(.*)", line)
        if m:
            ids.add(heading_id(m.group(1).strip(), seen))
    return ids

def iter_markdown(root: pathlib.Path) -> Iterator[pathlib.Path]:
    for path in root.rglob("*.md"):
        if any(part in EXCLUDED for part in path.parts):
            continue
        posix = pathlib.PurePosixPath(path.as_posix())
        if PRESERVED in posix.parents and posix != PRESERVED / "README.md":
            continue
        yield path


def prose_lines(path: pathlib.Path) -> list[str]:
    lines = []
    in_fence = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            lines.append("")
            continue
        if in_fence:
            lines.append("")
            continue
        lines.append(re.sub(r"`[^`]*`", "", line))
    return lines



def local_targets(path: pathlib.Path) -> list[tuple[int, str]]:
    out = []
    for i, line in enumerate(prose_lines(path), 1):
        for m in LINK_RE.finditer(line):
            out.append((i, m.group(1)))
    text = "\n".join(prose_lines(path))
    for m in REF_RE.finditer(text):
        lineno = text.count("\n", 0, m.start()) + 1
        out.append((lineno, m.group(1)))
    return out


def is_remote(target: str) -> bool:
    return "://" in target or target.startswith("mailto:")


def is_placeholder(target: str) -> bool:
    return "<" in target or ">" in target


def check(root: pathlib.Path) -> list[str]:
    errors = []
    cache = {}
    for path in sorted(iter_markdown(root)):
        rel = path.as_posix()
        for lineno, target in local_targets(path):
            if is_remote(target) or is_placeholder(target):
                continue
            if "#" in target:
                fpart, frag = target.split("#", 1)
            else:
                fpart, frag = target, ""
            if not fpart:
                if frag and frag not in cache.setdefault(rel, heading_ids(path)):
                    errors.append(f"{rel}:{lineno}: missing fragment #{frag}")
                continue
            if fpart.startswith("/"):
                errors.append(f"{rel}:{lineno}: absolute local path {target}")
                continue
            dest = os.path.normpath((path.parent / fpart).as_posix())
            if os.path.isdir(dest):
                continue
            if not os.path.exists(dest):
                errors.append(f"{rel}:{lineno}: missing file {target}")
                continue
            if frag:
                ids = cache.setdefault(dest, heading_ids(pathlib.Path(dest)))
                if frag not in ids:
                    errors.append(f"{rel}:{lineno}: missing fragment #{frag} in {target}")
    return errors


def main() -> int:
    root = pathlib.Path(".")
    errors = check(root)
    for error in errors:
        print(error)
    if errors:
        print(f"docs-links: {len(errors)} unresolved references", file=sys.stderr)
        return 1
    print("docs-links: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
