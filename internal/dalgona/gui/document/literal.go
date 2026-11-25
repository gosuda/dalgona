package document

import (
	"sort"
	"strconv"
	"strings"
)

func literalKind(format Format) Kind {
	switch format {
	case FormatCode:
		return KindCode
	case FormatNotice:
		return KindNotice
	case FormatPatch:
		return KindDiff
	case FormatMarkdown:
		return KindParagraph
	}
	return KindParagraph
}

type literalSection struct {
	start   int
	rows    []Span
	wrapped bool
}

type patchScan struct {
	header bool
	old    int
	fresh  int
}

func (s patchScan) inHunk() bool {
	return s.old > 0 || s.fresh > 0
}

func (s patchScan) opens(line string) bool {
	switch {
	case s.inHunk():
		return false
	case strings.HasPrefix(line, "diff "):
		return true
	case strings.HasPrefix(line, "--- "), strings.HasPrefix(line, "Binary files "):
		return !s.header
	}
	return false
}

func (s patchScan) next(line string) patchScan {
	if s.inHunk() {
		return s.hunkLine(line)
	}
	if strings.HasPrefix(line, "diff ") {
		return patchScan{header: true}
	}
	if old, fresh, ok := hunkCounts(line); ok {
		return patchScan{old: old, fresh: fresh}
	}
	if strings.HasPrefix(line, "--- ") || strings.HasPrefix(line, "Binary files ") {
		return patchScan{}
	}
	return s
}

func (s patchScan) hunkLine(line string) patchScan {
	switch {
	case line == "" || line[0] == ' ' || line[0] == '\n' || line[0] == '\r':
		s.old, s.fresh = s.old-1, s.fresh-1
	case line[0] == '-':
		s.old--
	case line[0] == '+':
		s.fresh--
	case line[0] == '\\':
	default:
		return patchScan{}
	}
	return patchScan{old: max(s.old, 0), fresh: max(s.fresh, 0)}
}

func hunkCounts(line string) (int, int, bool) {
	ranges, ok := strings.CutPrefix(line, "@@ -")
	if !ok {
		return 0, 0, false
	}
	old, ranges, ok := strings.Cut(ranges, " +")
	if !ok {
		return 0, 0, false
	}
	fresh, _, ok := strings.Cut(ranges, " @@")
	if !ok {
		return 0, 0, false
	}
	return rangeCount(old), rangeCount(fresh), true
}

func rangeCount(field string) int {
	_, count, found := strings.Cut(field, ",")
	if !found {
		return 1
	}
	n, err := strconv.Atoi(count)
	if err != nil || n < 0 {
		return 0
	}
	return n
}

type literalScan struct {
	blocks  []Block
	section literalSection
	patch   patchScan
}

func (d *Document) literalBlocks(kept []Block, resume *Block, patch patchScan) []Block {
	s := literalScan{blocks: make([]Block, len(kept), len(kept)+1), patch: patch}
	copy(s.blocks, kept)
	lineStart := 0
	if resume != nil {
		s.section, lineStart = resumeSection(d.source, *resume)
	}
	for {
		lineEnd := len(d.source)
		if newline := strings.IndexByte(d.source[lineStart:], '\n'); newline >= 0 {
			lineEnd = lineStart + newline + 1
		}
		if lineEnd == len(d.source) {
			d.patch = s.patch
		}
		d.scanLine(&s, Span{Start: lineStart, End: lineEnd})
		if lineEnd == len(d.source) {
			return append(s.blocks, d.literalBlock(s.section, lineEnd))
		}
		lineStart = lineEnd
	}
}

func (d *Document) scanLine(s *literalScan, line Span) {
	if d.header.Format == FormatPatch {
		text := d.source[line.Start:line.End]
		split := s.patch.opens(text)
		if d.header.PatchProjection {
			split = strings.HasPrefix(text, "diff ")
		}
		if line.Start > s.section.start && split {
			s.blocks = append(s.blocks, d.literalBlock(s.section, line.Start))
			s.section = literalSection{start: line.Start}
		}
		if !d.header.PatchProjection {
			s.patch = s.patch.next(text)
		}
	}
	var wrapped bool
	s.section.rows, wrapped = boundedRows(d.source[s.section.start:], Span{Start: line.Start - s.section.start, End: line.End - s.section.start}, s.section.rows)
	s.section.wrapped = s.section.wrapped || wrapped
}

func resumeSection(source string, last Block) (literalSection, int) {
	text := source[last.Span.Start:]
	line := strings.LastIndexByte(text[:last.Rows[len(last.Rows)-1].Start], '\n') + 1
	keep := sort.Search(len(last.Rows), func(i int) bool { return last.Rows[i].Start >= line })
	rows := make([]Span, keep, keep+1)
	copy(rows, last.Rows)
	return literalSection{start: last.Span.Start, rows: rows}, last.Span.Start + line
}

func (d *Document) literalBlock(section literalSection, end int) Block {
	text := d.source[section.start:end]
	block := Block{
		Kind:       literalKind(d.header.Format),
		Span:       Span{Start: section.start, End: end},
		Language:   d.header.Language,
		Projection: Projection{Text: text},
		Body:       Span{Start: 0, End: len(text)},
		Rows:       section.rows,
	}
	if text != "" {
		block.Projection.Runs = []Run{{Text: block.Body, Source: block.Span, Kind: Verbatim}}
	}
	if section.wrapped && !d.hasDiagnostic(LimitRow) {
		d.diagnostics = append(append([]Diagnostic(nil), d.diagnostics...), rowDiagnostic(Span{Start: section.start, End: section.start}))
	}
	return block
}

func (d *Document) hasDiagnostic(limit Limit) bool {
	for _, diagnostic := range d.diagnostics {
		if diagnostic.Limit == limit {
			return true
		}
	}
	return false
}
