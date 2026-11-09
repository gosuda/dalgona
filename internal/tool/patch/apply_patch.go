package patch

import (
	"errors"
	"fmt"
	"strings"
)

type patchChunk struct {
	context string
	old     string
	newBody string
	atEOF   bool
}

type applyParser struct {
	lines   []framedLine
	end     int
	edits   []edit
	targets map[string]bool
}

func parseApplyPatch(input string) ([]edit, error) {
	lines := splitFramed(stripOuterFence(input))
	if len(lines) == 0 || lines[0].text != "*** Begin Patch" || !lines[0].terminated {
		return nil, errors.New("invalid patch: The first line of the patch must be '*** Begin Patch'")
	}
	end := len(lines) - 1
	if lines[end].text != "*** End Patch" {
		return nil, errors.New("invalid patch: The last line of the patch must be '*** End Patch'")
	}
	parser := applyParser{lines: lines, end: end, targets: make(map[string]bool)}
	for cursor := 1; cursor < end; {
		next, err := parser.operation(cursor)
		if err != nil {
			return nil, err
		}
		cursor = next
	}
	return parser.edits, nil
}

func (p *applyParser) operation(cursor int) (int, error) {
	text := p.lines[cursor].text
	for _, op := range []struct {
		prefix string
		parse  func(path string, cursor int) (int, error)
	}{
		{"*** Add File: ", p.addFile},
		{"*** Delete File: ", p.deleteFile},
		{"*** Update File: ", p.updateFile},
	} {
		if raw, ok := strings.CutPrefix(text, op.prefix); ok {
			path, err := p.claim(raw, cursor+1)
			if err != nil {
				return 0, err
			}
			return op.parse(path, cursor)
		}
	}
	return 0, invalidHunk(cursor+1, "invalid file operation header")
}

func (p *applyParser) claim(raw string, line int) (string, error) {
	path, err := patchFilename(raw, line)
	if err != nil {
		return "", err
	}
	if p.targets[path] {
		return "", fmt.Errorf("multiple operations target %s", path)
	}
	p.targets[path] = true
	return path, nil
}

func (p *applyParser) addFile(path string, cursor int) (int, error) {
	body, next, err := parseAddBody(p.lines, cursor+1, p.end)
	if err != nil {
		return 0, err
	}
	p.edits = append(p.edits, edit{kind: editCreate, index: len(p.edits), path: path, body: body})
	return next, nil
}

func (p *applyParser) deleteFile(path string, cursor int) (int, error) {
	p.edits = append(p.edits, edit{kind: editDelete, index: len(p.edits), path: path})
	return cursor + 1, nil
}

func (p *applyParser) updateFile(path string, cursor int) (int, error) {
	cursor++
	rename := ""
	if cursor < p.end {
		if raw, ok := strings.CutPrefix(p.lines[cursor].text, "*** Move to: "); ok {
			var err error
			if rename, err = patchFilename(raw, cursor+1); err != nil {
				return 0, err
			}
			cursor++
		}
	}
	next, chunks, err := p.chunks(path, cursor)
	if err != nil {
		return 0, err
	}
	if chunks == 0 && rename == "" {
		return 0, invalidHunk(next+1, fmt.Sprintf("Update file hunk for path '%s' is empty", path))
	}
	if rename != "" {
		p.edits = append(p.edits, edit{kind: editRename, index: len(p.edits), path: path, to: rename})
	}
	return next, nil
}

func (p *applyParser) chunks(path string, cursor int) (int, int, error) {
	count := 0
	for cursor < p.end && !fileOperation(p.lines[cursor].text) {
		if !strings.HasPrefix(p.lines[cursor].text, "@@") {
			return 0, 0, invalidHunk(cursor+1, "Expected update hunk to start with a @@ context marker")
		}
		chunk, next, err := parseChunk(p.lines, cursor, p.end)
		if err != nil {
			return 0, 0, err
		}
		p.edits = append(p.edits, chunkEdit(len(p.edits), path, chunk))
		count++
		cursor = next
	}
	return cursor, count, nil
}

func patchFilename(value string, line int) (string, error) {
	if value == "" || strings.ContainsAny(value, "\r\n") {
		return "", invalidHunk(line, fmt.Sprintf("invalid file path '%s'", value))
	}
	return value, nil
}

func parseAddBody(lines []framedLine, cursor, end int) (string, int, error) {
	start := cursor
	var body strings.Builder
	for cursor < end && strings.HasPrefix(lines[cursor].text, "+") {
		if !lines[cursor].terminated {
			return "", 0, invalidHunk(cursor+1, "add line is missing LF")
		}
		body.WriteString(lines[cursor].text[1:])
		body.WriteByte('\n')
		cursor++
	}
	if cursor == start {
		return "", 0, invalidHunk(cursor+1, "add file needs at least one line")
	}
	return body.String(), cursor, nil
}

func fileOperation(text string) bool {
	return strings.HasPrefix(text, "*** Add File: ") ||
		strings.HasPrefix(text, "*** Delete File: ") || strings.HasPrefix(text, "*** Update File: ")
}

func parseChunk(lines []framedLine, start, end int) (patchChunk, int, error) {
	chunk := patchChunk{context: strings.TrimSpace(strings.TrimPrefix(lines[start].text, "@@"))}
	var oldBody, newBody strings.Builder
	cursor := start + 1
	for ; cursor < end && !chunkBoundary(lines[cursor].text); cursor++ {
		if err := appendChunkRow(&oldBody, &newBody, lines[cursor].text, cursor+1); err != nil {
			return patchChunk{}, 0, err
		}
	}
	if cursor == start+1 {
		return patchChunk{}, 0, invalidHunk(start+1, "Update hunk does not contain any lines")
	}
	if cursor < end && lines[cursor].text == "*** End of File" {
		chunk.atEOF = true
		cursor++
		if cursor != end && !fileOperation(lines[cursor].text) {
			return patchChunk{}, 0, invalidHunk(cursor+1, "*** End of File is not at end of update")
		}
	}
	chunk.old, chunk.newBody = oldBody.String(), newBody.String()
	return chunk, cursor, nil
}

func chunkBoundary(text string) bool {
	return strings.HasPrefix(text, "@@") || fileOperation(text) || text == "*** End of File"
}

func appendChunkRow(oldBody, newBody *strings.Builder, text string, line int) error {
	if text == "" {
		return invalidHunk(line, "Unexpected empty line in update hunk")
	}
	body := text[1:]
	switch text[0] {
	case ' ':
		appendPatchLine(oldBody, body)
		appendPatchLine(newBody, body)
	case '+':
		appendPatchLine(newBody, body)
	case '-':
		appendPatchLine(oldBody, body)
	default:
		return invalidHunk(line, "Every line should start with ' ', '+', or '-'")
	}
	return nil
}

func appendPatchLine(target *strings.Builder, line string) {
	target.WriteString(line)
	target.WriteByte('\n')
}

func chunkEdit(index int, path string, chunk patchChunk) edit {
	base := edit{kind: editChange, index: index, path: path, action: actionReplace, guard: guard{kind: guardQuoted}, body: chunk.newBody, window: afterPrevious}
	if chunk.old == "" && chunk.newBody != "" {
		base.locator = locator{kind: locatorGap, before: tailGap}
		base.action = actionInsertAfter
		base.guard.kind = guardExists
		return base
	}
	base.locator = locator{kind: locatorText, old: chunk.old, context: chunk.context, atEOF: chunk.atEOF}
	return base
}

func invalidHunk(line int, message string) error {
	return fmt.Errorf("invalid hunk at line %d, %s", line, message)
}
