package patch

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const tailGap = int(^uint(0) >> 1)

const newFileOnlyPut = "patch: a new file takes only PUT >$: rows"

type hashSection struct {
	path      string
	reference string
	isNew     bool
	base      int
	edits     []edit
	removed   bool
	renamed   bool
}

type hashParser struct {
	lines []framedLine
	style Style
	seen  map[string]string
	edits []edit
}

func parseHashline(input string, style Style) ([]edit, error) {
	strip, split := stripBeginEnd, splitFramed
	if style.profile() {
		strip, split = stripMatchedBeginEnd, splitFramedCRLF
	}
	input = strip(stripOuterFence(input))
	if input == "" {
		return nil, expected(1, "a hashline file header", "")
	}
	parser := hashParser{lines: split(input), style: style, seen: make(map[string]string)}
	for cursor := 0; cursor < len(parser.lines); {
		next, err := parser.section(cursor)
		if err != nil {
			return nil, err
		}
		cursor = next
	}
	return parser.edits, nil
}

func (p *hashParser) section(cursor int) (int, error) {
	section, err := openHashSection(p.lines[cursor].text, cursor+1, p.style)
	if err != nil {
		return 0, err
	}
	if claimErr := p.claim(section, cursor+1); claimErr != nil {
		return 0, claimErr
	}
	section.base = len(p.edits)
	cursor++
	for cursor < len(p.lines) && !isHashHeader(p.lines[cursor].text) {
		next, opErr := p.operation(cursor, section)
		if opErr != nil {
			return 0, opErr
		}
		cursor = next
	}
	if len(section.edits) == 0 {
		return 0, expected(cursor+1, "PUT, CUT, REM, or MV", "")
	}
	return cursor, p.finish(section)
}

func (p *hashParser) claim(section *hashSection, line int) error {
	previous, ok := p.seen[section.path]
	if ok && !p.style.profile() {
		return fmt.Errorf("patch: line %d: duplicate file section for %s", line, section.path)
	}
	if ok && previous != section.reference {
		return fmt.Errorf("patch: mixed_references for %s", section.path)
	}
	p.seen[section.path] = section.reference
	return nil
}

func (p *hashParser) finish(section *hashSection) error {
	if section.removed && len(section.edits) > 1 {
		return fmt.Errorf("patch: [%s]: REM cannot be combined with other ops on the same file", section.path)
	}
	if section.isNew && (len(section.edits) != 1 || section.edits[0].kind != editCreate) {
		return fmt.Errorf("patch: [%s#NEW]: a new file takes only PUT >$: rows", section.path)
	}
	p.edits = append(p.edits, section.edits...)
	return nil
}

func (p *hashParser) operation(cursor int, section *hashSection) (int, error) {
	text := p.lines[cursor].text
	line := cursor + 1
	if section.renamed && p.style.profile() {
		return 0, expected(line, "a new file section", text)
	}
	verb, rest, _ := strings.Cut(text, " ")
	switch verb {
	case "PUT":
		return p.put(section, rest, cursor)
	case "CUT":
		return cursor + 1, cut(section, rest, line)
	case "MV":
		return cursor + 1, p.move(section, rest, line)
	case "REM":
		if text == "REM" {
			return cursor + 1, remove(section)
		}
	}
	if strings.Contains(text, "*") {
		return 0, blockOpError(line)
	}
	return 0, expected(line, "PUT, CUT, REM, or MV", text)
}

func (p *hashParser) put(section *hashSection, rest string, cursor int) (int, error) {
	loc, act, err := parsePut(rest, cursor+1)
	if err != nil {
		return 0, err
	}
	if section.isNew && rest != ">$:" {
		return 0, fmt.Errorf("patch: [%s#NEW]: a new file takes only PUT >$: rows", section.path)
	}
	rows, next, err := readHashRows(p.lines, cursor+1)
	if err != nil {
		return 0, err
	}
	body := rows.body()
	if section.isNew && p.style.profile() {
		body = rows.newFileBody()
	}
	section.change(loc, act, body)
	return next, nil
}

func cut(section *hashSection, rest string, line int) error {
	if section.isNew {
		return errors.New(newFileOnlyPut)
	}
	first, last, valid := parseRange(rest)
	if !valid {
		return expected(line, "a valid line range", rest)
	}
	section.change(locator{kind: locatorLines, first: first, last: last}, actionReplace, "")
	return nil
}

func remove(section *hashSection) error {
	if section.isNew {
		return errors.New(newFileOnlyPut)
	}
	section.removed = true
	section.edits = append(section.edits, edit{kind: editDelete, index: section.next(), path: section.path, reference: section.reference})
	return nil
}

func (p *hashParser) move(section *hashSection, rest string, line int) error {
	if section.isNew || strings.TrimSpace(rest) == "" {
		return expected(line, "a destination filename", "MV "+rest)
	}
	dest, err := profileDestination(rest, line, p.style)
	if err != nil {
		return err
	}
	section.renamed = true
	section.edits = append(section.edits, edit{kind: editRename, index: section.next(), path: section.path, to: dest, reference: section.reference})
	return nil
}

func blockOpError(line int) error {
	return fmt.Errorf("patch: line %d: block ops (N*) are not supported; use a line range", line)
}

func (s *hashSection) next() int {
	return s.base + len(s.edits)
}

func (s *hashSection) change(loc locator, act action, body string) {
	if s.isNew {
		s.edits = append(s.edits, edit{kind: editCreate, index: s.next(), path: s.path, body: body})
		return
	}
	proof := guard{kind: guardVersion, text: s.reference}
	if snapshot.ValidRef(s.reference) {
		proof = guard{kind: guardReference, text: s.reference}
	}
	s.edits = append(s.edits, edit{
		kind: editChange, index: s.next(), path: s.path, locator: loc,
		action: act, guard: proof, body: body, window: beforePayload,
	})
}

func isHashHeader(text string) bool {
	return strings.HasPrefix(text, "[") && strings.HasSuffix(text, "]")
}

func openHashSection(text string, line int, style Style) (*hashSection, error) {
	inner, ok := strings.CutPrefix(text, "[")
	if !ok {
		return nil, expected(line, "a hashline file header", text)
	}
	inner, ok = strings.CutSuffix(inner, "]")
	if !ok {
		return nil, expected(line, "a hashline file header", text)
	}
	if style.profile() {
		return openProfileSection(inner, line, text)
	}
	path, tag, ok := strings.Cut(inner, "#")
	valid := ok && path != "" && !strings.ContainsAny(path, "#\r\n")
	isNew := tag == "NEW"
	if !valid || !isNew && !upperHex(tag, 4) {
		return nil, expected(line, "a [path#TAG] header", text)
	}
	return &hashSection{path: path, reference: tag, isNew: isNew}, nil
}

func openProfileSection(inner string, line int, original string) (*hashSection, error) {
	path, suffix, ok := splitProfileHeader(inner)
	if !ok {
		return nil, expected(line, "a [path@reference] header", original)
	}
	if suffix == "#NEW" {
		return &hashSection{path: path, isNew: true}, nil
	}
	if ref, isRef := strings.CutPrefix(suffix, "@"); isRef && snapshot.ValidRef(ref) {
		return &hashSection{path: path, reference: ref}, nil
	}
	if strings.HasPrefix(suffix, "#") {
		return nil, errors.New("patch: Light and Enhanced profiles do not accept [PATH#TAG]; copy the [path@reference] header from read")
	}
	return nil, expected(line, "a [path@reference] header", original)
}

func splitProfileHeader(inner string) (string, string, bool) {
	if strings.HasPrefix(inner, "\"") {
		return splitQuotedHeader(inner)
	}
	split := max(strings.LastIndex(inner, "@"), strings.LastIndex(inner, "#"))
	if split <= 0 {
		return "", "", false
	}
	path := inner[:split]
	return path, inner[split:], utf8.ValidString(path) && snapshot.HeaderPath(path) == path
}

func splitQuotedHeader(inner string) (string, string, bool) {
	decoder := jsontext.NewDecoder(strings.NewReader(inner))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != '"' {
		return "", "", false
	}
	path := token.String()
	end := decoder.InputOffset()
	if path == "" || strings.IndexByte(path, 0) >= 0 {
		return "", "", false
	}
	return path, inner[end:], true
}

func profileDestination(rest string, line int, style Style) (string, error) {
	rest = strings.TrimSpace(rest)
	if !style.profile() || !strings.HasPrefix(rest, "\"") {
		return rest, nil
	}
	value, err := jsonString([]byte(rest))
	if err != nil || value == "" {
		return "", expected(line, "a destination filename", rest)
	}
	return value, nil
}

func parsePut(rest string, line int) (locator, action, error) {
	text, ok := strings.CutSuffix(rest, ":")
	if !ok {
		return locator{}, 0, expected(line, "PUT with a trailing colon", rest)
	}
	if text == ">$" {
		return locator{kind: locatorGap, before: tailGap}, actionInsertAfter, nil
	}
	if strings.HasPrefix(text, "<") || strings.HasPrefix(text, ">") {
		return parsePutGap(text, line)
	}
	if strings.Contains(text, "*") {
		return locator{}, 0, blockOpError(line)
	}
	first, last, valid := parseRange(text)
	if !valid {
		return locator{}, 0, expected(line, "a valid line range", text)
	}
	return locator{kind: locatorLines, first: first, last: last}, actionReplace, nil
}

func parsePutGap(text string, line int) (locator, action, error) {
	number, valid := positiveNumber(text[1:])
	if !valid {
		return locator{}, 0, expected(line, "a positive line number", text[1:])
	}
	if text[0] == '<' {
		return locator{kind: locatorGap, before: number}, actionInsertBefore, nil
	}
	return locator{kind: locatorGap, before: number + 1}, actionInsertAfter, nil
}

type hashRows struct {
	rows       []string
	terminated bool
}

func readHashRows(lines []framedLine, cursor int) (hashRows, int, error) {
	start := cursor
	var out hashRows
	for ; cursor < len(lines); cursor++ {
		row, ok := strings.CutPrefix(lines[cursor].text, "+")
		if !ok {
			break
		}
		out.rows = append(out.rows, row)
		out.terminated = lines[cursor].terminated
	}
	if len(out.rows) == 0 {
		return hashRows{}, 0, expected(start+1, "a + body row", "")
	}
	return out, cursor, nil
}

func (r hashRows) body() string {
	return joinBody(r.rows, r.terminated)
}

func (r hashRows) newFileBody() string {
	return strings.Join(r.rows, "\n")
}
