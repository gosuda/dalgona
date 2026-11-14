package patch

import (
	"cmp"
	"errors"
	"fmt"
	"strings"
)

type anchorMarker int

const (
	markerNone anchorMarker = iota
	markerFile
	markerNew
	markerDelete
	markerMove
	markerFind
	markerReplaceFile
	markerAction
)

type anchorLine struct {
	kind   anchorMarker
	value  string
	action action
}

type anchorParser struct {
	lines []framedLine
	edits []edit
	path  string
	tag   string
}

func parseAnchor(input string) ([]edit, error) {
	lines := splitFramed(stripEnvelope(stripOuterFence(input)))
	if len(lines) == 0 {
		return nil, errors.New("patch: input must start with a *** File:, *** New File:, *** Delete File:, or *** Move: header")
	}
	parser := anchorParser{lines: lines}
	for cursor := 0; cursor < len(lines); {
		next, err := parser.step(cursor)
		if err != nil {
			return nil, err
		}
		cursor = next
	}
	return parser.edits, nil
}

func (p *anchorParser) step(cursor int) (int, error) {
	marker := classifyAnchor(p.lines[cursor].text)
	switch marker.kind {
	case markerFile:
		return p.openFile(marker.value, cursor)
	case markerNew:
		return p.newFile(marker.value, cursor)
	case markerDelete:
		return p.deleteFile(marker.value, cursor)
	case markerMove:
		return p.moveFile(marker.value, cursor)
	case markerReplaceFile:
		return p.replaceFile(cursor)
	case markerFind:
		return p.find(marker.value, cursor)
	case markerNone, markerAction:
	}
	return 0, expected(cursor+1, "a header", p.lines[cursor].text)
}

func (p *anchorParser) openFile(value string, cursor int) (int, error) {
	path, tag, err := parseAnchorFile(value, cursor+1)
	if err != nil {
		return 0, err
	}
	if path == "" && p.path == "" {
		return 0, fmt.Errorf("patch: line %d: a bare *** File: needs an earlier *** File: path", cursor+1)
	}
	p.path = cmp.Or(path, p.path)
	p.tag = cmp.Or(tag, p.tag)
	return cursor + 1, nil
}

func (p *anchorParser) newFile(path string, cursor int) (int, error) {
	if path == "" {
		return 0, expected(cursor+1, "*** New File: path", path)
	}
	body, next := collectAnchorBody(p.lines, cursor+1)
	p.edits = append(p.edits, edit{kind: editCreate, index: len(p.edits), path: path, body: body})
	return next, nil
}

func (p *anchorParser) deleteFile(path string, cursor int) (int, error) {
	if path == "" {
		return 0, expected(cursor+1, "*** Delete File: path", path)
	}
	p.edits = append(p.edits, edit{kind: editDelete, index: len(p.edits), path: path})
	return cursor + 1, nil
}

func (p *anchorParser) moveFile(value string, cursor int) (int, error) {
	from, to, ok := strings.Cut(value, "->")
	from, to = strings.TrimSpace(from), strings.TrimSpace(to)
	if !ok || from == "" || to == "" {
		return 0, expected(cursor+1, "*** Move: old -> new", value)
	}
	p.edits = append(p.edits, edit{kind: editRename, index: len(p.edits), path: from, to: to})
	return cursor + 1, nil
}

func (p *anchorParser) replaceFile(cursor int) (int, error) {
	if p.path == "" {
		return 0, expected(cursor+1, "a file header", p.lines[cursor].text)
	}
	body, next := collectAnchorBody(p.lines, cursor+1)
	p.change(locator{kind: locatorWhole}, actionReplace, body)
	return next, nil
}

func (p *anchorParser) find(selector string, cursor int) (int, error) {
	if p.path == "" {
		return 0, expected(cursor+1, "a file header", p.lines[cursor].text)
	}
	quoted, next := collectFindBody(p.lines, cursor+1)
	if next >= len(p.lines) {
		return 0, fmt.Errorf("patch: changes[%d]: *** Find needs an action", len(p.edits))
	}
	marker := classifyAnchor(p.lines[next].text)
	if marker.kind != markerAction {
		return 0, fmt.Errorf("patch: changes[%d]: *** Find needs an action", len(p.edits))
	}
	body, end := collectAnchorBody(p.lines, next+1)
	loc, err := anchorLocator(selector, quoted, len(p.edits))
	if err != nil {
		return 0, err
	}
	p.change(loc, marker.action, body)
	return end, nil
}

func (p *anchorParser) change(loc locator, act action, body string) {
	proof := guard{kind: guardSeen}
	if p.tag != "" {
		proof = guard{kind: guardWholeTag, text: p.tag}
	} else if loc.kind == locatorText && !loc.all {
		proof.kind = guardQuoted
	}
	p.edits = append(p.edits, edit{
		kind: editChange, index: len(p.edits), path: p.path, locator: loc,
		action: act, guard: proof, body: body, window: beforePayload,
	})
}

func classifyAnchor(text string) anchorLine {
	trimmed := strings.Trim(text, " \t")
	for _, item := range []struct {
		prefix string
		kind   anchorMarker
	}{{"*** File:", markerFile}, {"*** New File:", markerNew}, {"*** Delete File:", markerDelete}, {"*** Move:", markerMove}, {"*** Find", markerFind}} {
		if rest, ok := strings.CutPrefix(trimmed, item.prefix); ok {
			return anchorLine{kind: item.kind, value: strings.TrimSpace(rest)}
		}
	}
	switch trimmed {
	case "*** Replace File":
		return anchorLine{kind: markerReplaceFile}
	case "*** Replace":
		return anchorLine{kind: markerAction, action: actionReplace}
	case "*** Insert Before":
		return anchorLine{kind: markerAction, action: actionInsertBefore}
	case "*** Insert After":
		return anchorLine{kind: markerAction, action: actionInsertAfter}
	default:
		return anchorLine{}
	}
}

func parseAnchorFile(value string, line int) (string, string, error) {
	path, tag, found := strings.Cut(value, "#")
	path = strings.TrimSpace(path)
	if found && (path == "" || !upperHex(tag, 8)) {
		return "", "", expected(line, "*** File: path [#TAG]", value)
	}
	return path, tag, nil
}

func collectBodyWhile(lines []framedLine, cursor int, match func(anchorMarker) bool) (string, int) {
	var body strings.Builder
	for cursor < len(lines) && match(classifyAnchor(lines[cursor].text).kind) {
		body.WriteString(lines[cursor].raw)
		cursor++
	}
	return body.String(), cursor
}

func collectFindBody(lines []framedLine, cursor int) (string, int) {
	return collectBodyWhile(lines, cursor, func(kind anchorMarker) bool { return kind != markerAction })
}

func collectAnchorBody(lines []framedLine, cursor int) (string, int) {
	return collectBodyWhile(lines, cursor, func(kind anchorMarker) bool { return kind == markerNone })
}

func anchorLocator(selector, quoted string, index int) (locator, error) {
	switch selector {
	case "", "all":
		return locator{kind: locatorText, old: quoted, all: selector == "all"}, nil
	}
	if line, ok := strings.CutPrefix(selector, "@"); ok {
		number, valid := positiveNumber(line)
		if !valid {
			return locator{}, fmt.Errorf("patch: changes[%d]: invalid line selector %q", index, selector)
		}
		return locator{kind: locatorText, old: quoted, lineHint: number}, nil
	}
	first, last, ok := parseRange(selector)
	if !ok {
		return locator{}, fmt.Errorf("patch: changes[%d]: invalid span selector %q", index, selector)
	}
	lines := viewLines(quoted)
	rows := make([]string, len(lines))
	for i, line := range lines {
		rows[i] = quoted[line.start:line.contentEnd]
	}
	span := last - first + 1
	if len(rows) != 2 && len(rows) != span {
		return locator{}, fmt.Errorf("patch: changes[%d]: *** Find %d-%d quotes %d lines; quote 2 lines (first and last) or all %d lines", index, first, last, len(rows), span)
	}
	return locator{kind: locatorSpan, first: first, last: last, quoted: rows}, nil
}
