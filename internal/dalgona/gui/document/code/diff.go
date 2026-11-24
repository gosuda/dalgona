package code

import (
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"

	udiff "github.com/aymanbagabas/go-udiff"
)

const (
	MaxDiffBytes = 1 << 20
	DiffContext  = 3
)

type Op uint8

const (
	ContextOp Op = iota
	AddOp
	DeleteOp
)

type FileKind uint8

const (
	Modify FileKind = iota
	Add
	Delete
	Rename
	Binary
	Missing
	Omitted
)

type Line struct {
	Op   Op
	Old  int
	New  int
	Text string
}

type Hunk struct {
	OldStart int
	OldCount int
	NewStart int
	NewCount int
	Lines    []Line
}

type File struct {
	OldName string
	NewName string
	Kind    FileKind
	Hunks   []Hunk
	Omitted string
}

func (f File) Additions() int {
	total := 0
	for _, h := range f.Hunks {
		for _, l := range h.Lines {
			if l.Op == AddOp {
				total++
			}
		}
	}
	return total
}

func (f File) Deletions() int {
	total := 0
	for _, h := range f.Hunks {
		for _, l := range h.Lines {
			if l.Op == DeleteOp {
				total++
			}
		}
	}
	return total
}

func OmittedFile(oldName, newName, reason string) File {
	return File{OldName: oldName, NewName: newName, Kind: Omitted, Omitted: reason}
}

func (f File) Patch() string {
	switch f.Kind {
	case Omitted:
		return fmt.Sprintf("--- %s\n+++ %s\ndiff omitted: %s\n", f.OldName, f.NewName, f.Omitted)
	case Binary:
		return fmt.Sprintf("Binary files %s and %s differ\n", f.OldName, f.NewName)
	case Rename:
		return fmt.Sprintf("rename from %s\nrename to %s\n", f.OldName, f.NewName)
	case Modify, Add, Delete, Missing:
	}
	if len(f.Hunks) == 0 {
		return ""
	}
	var out strings.Builder
	out.WriteString("--- " + f.OldName + "\n")
	out.WriteString("+++ " + f.NewName + "\n")
	for _, h := range f.Hunks {
		writeHunk(&out, h)
	}
	return out.String()
}

func writeHunk(out *strings.Builder, h Hunk) {
	out.WriteString("@@ -" + hunkRange(h.OldStart, h.OldCount) + " +" + hunkRange(h.NewStart, h.NewCount) + " @@\n")
	for _, l := range h.Lines {
		out.WriteString(hunkPrefix(l.Op) + l.Text + "\n")
	}
}

func hunkPrefix(op Op) string {
	switch op {
	case AddOp:
		return "+"
	case DeleteOp:
		return "-"
	case ContextOp:
		return " "
	}
	return " "
}

func hunkRange(start, count int) string {
	if count == 1 {
		return strconv.Itoa(start)
	}
	return strconv.Itoa(start) + "," + strconv.Itoa(count)
}

type SyntaxError struct {
	What string
}

func (e *SyntaxError) Error() string {
	return fmt.Sprintf("code: refuse diff: %s; send a unified diff with ---/+++/@@ headers", e.What)
}

type MismatchError struct {
	Hunk int
	Line int
}

func (e *MismatchError) Error() string {
	return fmt.Sprintf("code: refuse patch of hunk %d at old line %d: context does not match the source", e.Hunk, e.Line)
}

type DiffBudgetError struct {
	OldName string
	NewName string
	Bytes   int
	Limit   int
}

func (e *DiffBudgetError) Error() string {
	return fmt.Sprintf("code: diff-budget: %s to %s is %d bytes over the %d-byte diff limit; showing the omitted state", e.OldName, e.NewName, e.Bytes, e.Limit)
}

func Compute(oldName, newName, before, after string) (File, error) {
	if !utf8.ValidString(before) || !utf8.ValidString(after) {
		return File{OldName: oldName, NewName: newName}, &SyntaxError{What: "invalid UTF-8 in diff input"}
	}
	if len(before) > MaxDiffBytes || len(after) > MaxDiffBytes {
		biggest := len(before)
		if len(after) > biggest {
			biggest = len(after)
		}
		return OmittedFile(oldName, newName, fmt.Sprintf("%d bytes over the %d-byte diff limit", biggest, MaxDiffBytes)),
			&DiffBudgetError{OldName: oldName, NewName: newName, Bytes: biggest, Limit: MaxDiffBytes}
	}
	edits := udiff.Lines(before, after)
	if len(edits) == 0 {
		return File{OldName: oldName, NewName: newName, Kind: Modify}, nil
	}
	text, err := udiff.ToUnified(oldName, newName, before, edits, DiffContext)
	if err != nil {
		return File{OldName: oldName, NewName: newName}, &SyntaxError{What: "diff engine rejected its own edits"}
	}
	file, err := ParseUnified(text)
	if err != nil {
		return File{OldName: oldName, NewName: newName}, err
	}
	return file, nil
}

func ParseUnified(text string) (File, error) {
	if !utf8.ValidString(text) {
		return File{}, &SyntaxError{What: "invalid UTF-8 in diff text"}
	}
	if len(text) > MaxDiffBytes {
		return OmittedFile("", "", fmt.Sprintf("%d bytes over the %d-byte diff limit", len(text), MaxDiffBytes)),
			&DiffBudgetError{Bytes: len(text), Limit: MaxDiffBytes}
	}
	lines := splitDiffLines(text)
	if len(lines) == 0 {
		return File{Kind: Modify}, nil
	}
	if file, handled, err := parseSpecialFile(lines); handled || err != nil {
		return file, err
	}
	file, rest, err := splitDiffHeaders(lines)
	if err != nil {
		return File{}, err
	}
	hunks, err := collectHunks(rest)
	if err != nil {
		return File{}, err
	}
	file.Hunks = hunks
	return file, nil
}

func splitDiffLines(text string) []string {
	lines := strings.Split(text, "\n")
	if len(lines) > 0 && lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	return lines
}

func parseSpecialFile(lines []string) (File, bool, error) {
	if rest, ok := cutPrefix(lines[0], "Binary files "); ok {
		names := strings.Split(rest, " and ")
		if len(names) != 2 || !strings.HasSuffix(names[1], " differ") {
			return File{}, true, &SyntaxError{What: "malformed binary diff marker"}
		}
		return File{OldName: names[0], NewName: strings.TrimSuffix(names[1], " differ"), Kind: Binary}, true, nil
	}
	if from, ok := cutPrefix(lines[0], "rename from "); ok {
		if len(lines) < 2 {
			return File{}, true, &SyntaxError{What: "rename without a target"}
		}
		to, ok := cutPrefix(lines[1], "rename to ")
		if !ok {
			return File{}, true, &SyntaxError{What: "rename without a target"}
		}
		return File{OldName: from, NewName: to, Kind: Rename}, true, nil
	}
	return File{}, false, nil
}

func splitDiffHeaders(lines []string) (File, []string, error) {
	if len(lines) < 2 {
		return File{}, nil, &SyntaxError{What: "diff without ---/+++ headers"}
	}
	oldName, ok := cutPrefix(lines[0], "--- ")
	if !ok {
		return File{}, nil, &SyntaxError{What: "diff without ---/+++ headers"}
	}
	newName, ok := cutPrefix(lines[1], "+++ ")
	if !ok {
		return File{}, nil, &SyntaxError{What: "diff without ---/+++ headers"}
	}
	file := File{OldName: oldName, NewName: newName, Kind: Modify}
	if oldName == "/dev/null" {
		file.Kind = Add
	}
	if newName == "/dev/null" {
		file.Kind = Delete
	}
	return file, lines[2:], nil
}

func collectHunks(rest []string) ([]Hunk, error) {
	var hunks []Hunk
	for len(rest) > 0 {
		header, ok := cutPrefix(rest[0], "@@ ")
		if !ok {
			return nil, &SyntaxError{What: "expected a @@ hunk header"}
		}
		hunk, consumed, err := parseHunk(header, rest[1:])
		if err != nil {
			return nil, err
		}
		hunks = append(hunks, hunk)
		rest = rest[1+consumed:]
	}
	return hunks, nil
}

func parseHunk(header string, lines []string) (Hunk, int, error) {
	hunk, err := parseHunkRanges(header)
	if err != nil {
		return Hunk{}, 0, err
	}
	consumed, err := appendHunkBody(&hunk, lines)
	if err != nil {
		return Hunk{}, 0, err
	}
	for consumed < len(lines) && strings.HasPrefix(lines[consumed], "\\") {
		consumed++
	}
	return hunk, consumed, nil
}

func parseHunkRanges(header string) (Hunk, error) {
	end := strings.Index(header, " @@")
	if end < 0 {
		return Hunk{}, &SyntaxError{What: "malformed @@ hunk header"}
	}
	ranges := strings.Fields(header[:end])
	if len(ranges) != 2 {
		return Hunk{}, &SyntaxError{What: "malformed @@ hunk header"}
	}
	oldStart, oldCount, err := parseRange(ranges[0], '-')
	if err != nil {
		return Hunk{}, err
	}
	newStart, newCount, err := parseRange(ranges[1], '+')
	if err != nil {
		return Hunk{}, err
	}
	return Hunk{OldStart: oldStart, OldCount: oldCount, NewStart: newStart, NewCount: newCount}, nil
}

func appendHunkBody(hunk *Hunk, lines []string) (int, error) {
	old, fresh := hunk.OldStart, hunk.NewStart
	i := 0
	for old-hunk.OldStart+fresh-hunk.NewStart < hunk.OldCount+hunk.NewCount && i < len(lines) {
		raw := lines[i]
		i++
		if strings.HasPrefix(raw, "\\") {
			continue
		}
		var err error
		old, fresh, err = appendHunkLine(hunk, raw, old, fresh)
		if err != nil {
			return 0, err
		}
	}
	if old-hunk.OldStart != hunk.OldCount || fresh-hunk.NewStart != hunk.NewCount {
		return 0, &SyntaxError{What: "hunk body does not match its @@ counts"}
	}
	return i, nil
}

func parseRange(field string, mark byte) (int, int, error) {
	if len(field) == 0 || field[0] != mark {
		return 0, 0, &SyntaxError{What: "malformed @@ hunk header"}
	}
	field = field[1:]
	start, count, _ := strings.Cut(field, ",")
	n, err := strconv.Atoi(start)
	if err != nil || n < 0 {
		return 0, 0, &SyntaxError{What: "malformed @@ hunk header"}
	}
	if count == "" {
		return n, 1, nil
	}

	c, err := strconv.Atoi(count)
	if err != nil || c < 0 {
		return 0, 0, &SyntaxError{What: "malformed @@ hunk header"}
	}
	return n, c, nil
}

func appendHunkLine(hunk *Hunk, raw string, old, fresh int) (int, int, error) {
	if raw == "" {
		return old, fresh, &SyntaxError{What: "hunk line without a prefix"}
	}
	body := raw[1:]
	switch raw[0] {
	case ' ':
		hunk.Lines = append(hunk.Lines, Line{Op: ContextOp, Old: old, New: fresh, Text: body})
		return old + 1, fresh + 1, nil
	case '-':
		hunk.Lines = append(hunk.Lines, Line{Op: DeleteOp, Old: old, Text: body})
		return old + 1, fresh, nil
	case '+':
		hunk.Lines = append(hunk.Lines, Line{Op: AddOp, New: fresh, Text: body})
		return old, fresh + 1, nil
	default:
		return old, fresh, &SyntaxError{What: "hunk line without a prefix"}
	}
}

func (f File) Apply(before string) (string, error) {
	oldLines := strings.Split(before, "\n")
	out := make([]string, 0, len(oldLines))
	cursor := 1
	for hi, h := range f.Hunks {
		for cursor < h.OldStart {
			out = append(out, oldLines[cursor-1])
			cursor++
		}
		next, nextCursor, err := applyHunkLines(out, oldLines, h, hi, cursor)
		if err != nil {
			return "", err
		}
		out, cursor = next, nextCursor
	}
	for cursor <= len(oldLines) {
		out = append(out, oldLines[cursor-1])
		cursor++
	}
	return strings.Join(out, "\n"), nil
}

func applyHunkLines(out, oldLines []string, h Hunk, hi, cursor int) ([]string, int, error) {
	for _, l := range h.Lines {
		switch l.Op {
		case ContextOp:
			if !matchOldLine(oldLines, l.Old, l.Text) {
				return nil, 0, &MismatchError{Hunk: hi, Line: l.Old}
			}
			out = append(out, l.Text)
			cursor = l.Old + 1
		case DeleteOp:
			if !matchOldLine(oldLines, l.Old, l.Text) {
				return nil, 0, &MismatchError{Hunk: hi, Line: l.Old}
			}
			cursor = l.Old + 1
		case AddOp:
			out = append(out, l.Text)
		}
	}
	return out, cursor, nil
}

func matchOldLine(oldLines []string, old int, text string) bool {
	return old >= 1 && old <= len(oldLines) && oldLines[old-1] == text
}

func (f File) MapOld(old int) (int, bool) {
	delta := 0
	for _, h := range f.Hunks {
		if old < h.OldStart {
			return old + delta, true
		}
		if old < h.OldStart+h.OldCount {
			for _, l := range h.Lines {
				if l.Op == ContextOp && l.Old == old {
					return l.New, true
				}
			}
			return 0, false
		}
		delta = h.NewStart + h.NewCount - (h.OldStart + h.OldCount)
	}
	return old + delta, true
}

func (f File) MapNew(fresh int) (int, bool) {
	delta := 0
	for _, h := range f.Hunks {
		if fresh < h.NewStart {
			return fresh + delta, true
		}
		if fresh < h.NewStart+h.NewCount {
			for _, l := range h.Lines {
				if l.Op == ContextOp && l.New == fresh {
					return l.Old, true
				}
			}
			return 0, false
		}
		delta = h.OldStart + h.OldCount - (h.NewStart + h.NewCount)
	}
	return fresh + delta, true
}

type Row struct {
	IsGap       bool
	Line        Line
	GapOldStart int
	GapOldCount int
	GapNewStart int
	GapNewCount int
}

func (f File) Rows(keep int) []Row {
	var rows []Row
	for _, h := range f.Hunks {
		rows = append(rows, hunkRows(h, keep)...)
	}
	return rows
}

func hunkRows(h Hunk, keep int) []Row {
	var rows []Row
	lead, core, trail := splitEdge(h.Lines, keep)
	if lead.hidden > 0 {
		rows = append(rows, Row{
			IsGap:       true,
			GapOldStart: lead.oldStart, GapOldCount: lead.hidden,
			GapNewStart: lead.newStart, GapNewCount: lead.hidden,
		})
	}
	for _, l := range core {
		rows = append(rows, Row{Line: l})
	}
	if trail.hidden > 0 {
		rows = append(rows, Row{
			IsGap:       true,
			GapOldStart: trail.oldStart, GapOldCount: trail.hidden,
			GapNewStart: trail.newStart, GapNewCount: trail.hidden,
		})
	}
	return rows
}

type edge struct {
	oldStart int
	newStart int
	hidden   int
}

func splitEdge(lines []Line, keep int) (edge, []Line, edge) {
	if keep < 0 {
		return edge{}, lines, edge{}
	}
	leadCount := 0
	for leadCount < len(lines) && lines[leadCount].Op == ContextOp {
		leadCount++
	}
	trailCount := 0
	for trailCount < len(lines)-leadCount && lines[len(lines)-1-trailCount].Op == ContextOp {
		trailCount++
	}
	lead, rest := trimFront(lines, leadCount, keep)
	trail, core := trimBack(rest, trailCount, keep)
	return lead, core, trail
}

func trimFront(lines []Line, count, keep int) (edge, []Line) {
	if count <= keep {
		return edge{}, lines
	}
	first := lines[0]
	return edge{oldStart: first.Old, newStart: first.New, hidden: count - keep}, lines[count-keep:]
}

func trimBack(lines []Line, count, keep int) (edge, []Line) {
	if count <= keep {
		return edge{}, lines
	}
	first := lines[len(lines)-count]
	return edge{oldStart: first.Old, newStart: first.New, hidden: count - keep}, lines[:len(lines)-count+keep]
}

func cutPrefix(s, prefix string) (string, bool) {
	if !strings.HasPrefix(s, prefix) {
		return "", false
	}
	return s[len(prefix):], true
}
