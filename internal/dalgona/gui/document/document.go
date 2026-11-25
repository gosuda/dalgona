package document

import (
	"fmt"
	"hash/fnv"
	"sort"
	"strings"
	"sync"
	"unicode/utf8"
)

type Format uint8

const (
	FormatMarkdown Format = iota + 1
	FormatCode
	FormatNotice
	FormatPatch
)

type Header struct {
	Identity        Identity
	Format          Format
	Language        string
	Open            bool
	PatchProjection bool
}

type Document struct {
	header      Header
	revision    Revision
	source      string
	sourceBytes int
	log         *sourceLog
	literal     bool
	blocks      []Block
	regions     []region
	definitions []definition
	diagnostics []Diagnostic
	patch       patchScan
}

type sourceLog struct {
	mu      sync.Mutex
	builder strings.Builder
}

func newSourceLog(source string) *sourceLog {
	log := &sourceLog{}
	log.builder.Grow(len(source))
	log.builder.WriteString(source)
	return log
}

func (l *sourceLog) extend(prefix, suffix string) (string, int, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.builder.Len() != len(prefix) {
		return "", 0, false
	}
	l.builder.WriteString(suffix)
	return l.builder.String(), l.builder.Cap(), true
}

func New(header Header, source string) (*Document, error) {
	if !header.Identity.valid() {
		return nil, fmt.Errorf("%w: %+v", ErrIdentity, header.Identity)
	}
	if header.Format < FormatMarkdown || header.Format > FormatPatch {
		return nil, fmt.Errorf("document format %d is not Markdown, code, notice, or patch", header.Format)
	}
	if header.PatchProjection && header.Format != FormatPatch {
		return nil, fmt.Errorf("patch projection requires patch format, got %d", header.Format)
	}
	if err := checkEncoding(source, 0); err != nil {
		return nil, err
	}
	log := newSourceLog(source)
	d := &Document{header: header, revision: 1, source: log.builder.String(), sourceBytes: log.builder.Cap(), log: log}
	d.build()
	return d, nil
}

func checkEncoding(text string, base int) error {
	if utf8.ValidString(text) {
		return nil
	}
	for i, r := range text {
		if r == utf8.RuneError {
			if _, width := utf8.DecodeRuneInString(text[i:]); width == 1 {
				return fmt.Errorf("%w: invalid byte at offset %d; the source must be decoded text", ErrEncoding, base+i)
			}
		}
	}
	return ErrEncoding
}

func (d *Document) Identity() Identity { return d.header.Identity }
func (d *Document) Format() Format     { return d.header.Format }
func (d *Document) Language() string   { return d.header.Language }
func (d *Document) Revision() Revision { return d.revision }
func (d *Document) Source() string     { return d.source }
func (d *Document) ResidentBytes() int { return d.sourceBytes }
func (d *Document) Open() bool         { return d.header.Open }
func (d *Document) Len() int           { return len(d.blocks) }
func (d *Document) Block(i int) Block  { return d.blocks[i] }

func (d *Document) Diagnostics() []Diagnostic {
	out := make([]Diagnostic, 0, len(d.diagnostics))
	out = append(out, d.diagnostics...)
	for _, r := range d.regions {
		out = append(out, r.diagnostics...)
	}
	return out
}

func (d *Document) build() {
	d.blocks, d.regions, d.definitions, d.diagnostics = nil, nil, nil, nil
	d.literal = false
	if d.header.Format == FormatMarkdown {
		if diagnostic, over := markdownOverLimit(d.source, 0); over {
			d.literal = true
			d.diagnostics = []Diagnostic{diagnostic}
		}
	}
	if d.header.Format != FormatMarkdown || d.literal {
		d.blocks = d.literalBlocks(nil, nil, patchScan{})
		d.finish(0)
		return
	}
	p := parseMarkdown(d.source, 0, len(d.source), nil)
	d.blocks, d.regions, d.definitions = p.blocks, p.regions, p.definitions
	d.finish(0)
}

func markdownOverLimit(source string, from int) (Diagnostic, bool) {
	if len(source) > MaxMarkdownBytes {
		return markdownSizeDiagnostic(len(source)), true
	}
	for start := strings.LastIndexByte(source[:from], '\n') + 1; start < len(source); {
		end := strings.IndexByte(source[start:], '\n')
		if end < 0 {
			end = len(source) - start
		}
		if end > MaxMarkdownLineBytes {
			return markdownLineDiagnostic(Span{Start: start, End: start + end}), true
		}
		start += end + 1
	}
	return Diagnostic{}, false
}

func (d *Document) finish(from int) {
	for i := from; i < len(d.blocks); i++ {
		d.blocks[i].Span.End = d.blockLimit(i)
	}
	tail := d.tailBlock()
	for i := from; i < len(d.blocks); i++ {
		provisional := d.header.Open && i >= tail && !d.closedFigure(i)
		d.blocks[i].Key = d.key(d.blocks[i], provisional)
	}
}

func (d *Document) closedFigure(i int) bool {
	b := d.blocks[i]
	if b.Kind != KindFigure {
		return false
	}
	return fenceClosed(d.source, b.Span.Start, b.Span.End)
}

func fenceClosed(source string, start, end int) bool {
	if start < 0 || end > len(source) || start >= end {
		return false
	}
	lines := strings.Split(source[start:end], "\n")
	mark, length := openingFence(lines[0])
	if length == 0 {
		return false
	}
	for _, line := range lines[1:] {
		if closingFence(line, mark, length) {
			return true
		}
	}
	return false
}

func stripFenceIndent(line string) (string, bool) {
	indent := 0
	for indent < len(line) && line[indent] == ' ' {
		indent++
	}
	if indent > 3 {
		return "", false
	}
	return line[indent:], true
}

func openingFence(line string) (byte, int) {
	stripped, ok := stripFenceIndent(line)
	if !ok || stripped == "" {
		return 0, 0
	}
	mark := stripped[0]
	if mark != '`' && mark != '~' {
		return 0, 0
	}
	length := 0
	for length < len(stripped) && stripped[length] == mark {
		length++
	}
	if length < 3 {
		return 0, 0
	}
	return mark, length
}

func closingFence(line string, mark byte, length int) bool {
	stripped, ok := stripFenceIndent(line)
	if !ok {
		return false
	}
	count := 0
	for count < len(stripped) && stripped[count] == mark {
		count++
	}
	return count >= length && strings.Trim(stripped[count:], " \t") == ""
}

func (d *Document) blockLimit(i int) int {
	limit := len(d.source)
	if i+1 < len(d.blocks) {
		limit = d.blocks[i+1].Span.Start
	}
	if d.literal || d.header.Format != FormatMarkdown {
		return limit
	}
	r := sort.Search(len(d.regions), func(r int) bool { return d.regions[r].first > i })
	if r < len(d.regions) {
		limit = min(limit, d.regions[r].start)
	}
	return limit
}

func (d *Document) tailBlock() int {
	if d.literal || d.header.Format != FormatMarkdown {
		return len(d.blocks) - 1
	}
	if len(d.regions) == 0 {
		return len(d.blocks)
	}
	return d.regions[len(d.regions)-1].first
}

func (d *Document) key(b Block, provisional bool) Key {
	if provisional {
		return Key{Document: d.header.Identity, Span: Span{Start: b.Span.Start, End: b.Span.Start}, Provisional: true}
	}
	hash := fnv.New64a()
	_, _ = hash.Write([]byte(d.source[b.Span.Start:b.Span.End]))
	return Key{Document: d.header.Identity, Span: b.Span, Kind: b.Kind, Hash: hash.Sum64()}
}

func (d *Document) successor() *Document {
	next := *d
	next.revision++
	return &next
}

func (d *Document) Append(text string) (*Document, Change, error) {
	if !d.header.Open {
		return d, Change{}, fmt.Errorf("%w: %+v accepts no more output", ErrClosed, d.header.Identity)
	}
	if err := checkEncoding(text, len(d.source)); err != nil {
		return d, Change{}, err
	}
	if text == "" {
		return d, Change{}, nil
	}
	next := d.successor()
	source, size, ok := d.log.extend(d.source, text)
	if !ok {
		next.log = newSourceLog(d.source + text)
		source, size = next.log.builder.String(), next.log.builder.Cap()
	}
	next.source, next.sourceBytes = source, size
	from := next.appendBlocks(d)
	return next, d.changeTo(next, nil, d.blocks[min(from, len(d.blocks)):]), nil
}

func (d *Document) changeTo(next *Document, edits []Edit, old []Block) Change {
	mapping := func(offset int) int { return offset }
	if edits != nil {
		mapping = func(offset int) int { return mapOffset(edits, offset, After) }
	}
	return Change{
		From:     d.header.Identity,
		To:       next.header.Identity,
		Revision: next.revision,
		Edits:    edits,
		Appended: edits == nil,
		Keys:     keyChanges(old, next, mapping),
	}
}

func (d *Document) appendBlocks(previous *Document) int {
	if d.header.Format == FormatMarkdown && !d.literal {
		if diagnostic, over := markdownOverLimit(d.source, len(previous.source)); over {
			d.literal = true
			d.diagnostics = []Diagnostic{diagnostic}
			d.regions, d.definitions = nil, nil
			d.blocks = d.literalBlocks(nil, nil, patchScan{})
			d.finish(0)
			return 0
		}
		return d.appendMarkdown(previous)
	}
	last := len(previous.blocks) - 1
	d.blocks = d.literalBlocks(previous.blocks[:last], &previous.blocks[last], previous.patch)
	d.finish(last)
	return last
}

func (d *Document) Close() (*Document, Change, error) {
	if !d.header.Open {
		return d, Change{}, fmt.Errorf("%w: %+v is already complete", ErrClosed, d.header.Identity)
	}
	next := d.successor()
	next.header.Open = false
	next.blocks = append([]Block(nil), d.blocks...)
	tail := max(d.tailBlock(), 0)
	next.finish(tail)
	return next, d.changeTo(next, nil, d.blocks[min(tail, len(d.blocks)):]), nil
}

func (d *Document) Replace(source string) (*Document, Change, error) {
	if err := checkEncoding(source, 0); err != nil {
		return d, Change{}, err
	}
	next := d.successor()
	next.log = newSourceLog(source)
	next.source, next.sourceBytes = next.log.builder.String(), next.log.builder.Cap()
	next.build()
	edits := diffEdits(d.source, source)
	if edits == nil {
		edits = []Edit{}
	}
	return next, d.changeTo(next, edits, d.blocks), nil
}

func (d *Document) Assign(committed Identity) (*Document, Change, error) {
	if !d.header.Identity.IsDraft() {
		return d, Change{}, fmt.Errorf("%w: %+v is already committed", ErrIdentity, d.header.Identity)
	}
	if committed.IsDraft() || !committed.valid() {
		return d, Change{}, fmt.Errorf("%w: the server identity %+v is not a committed identity", ErrIdentity, committed)
	}
	next := d.successor()
	next.header.Identity = committed
	next.blocks = append([]Block(nil), d.blocks...)
	next.finish(0)
	return next, d.changeTo(next, nil, d.blocks), nil
}

func keyChanges(old []Block, next *Document, mapping func(int) int) []KeyChange {
	var changes []KeyChange
	for _, b := range old {
		i, ok := next.blockAt(mapping(b.Span.Start))
		if !ok {
			continue
		}
		if to := next.blocks[i].Key; to != b.Key {
			changes = append(changes, KeyChange{From: b.Key, To: to})
		}
	}
	return changes
}

func (d *Document) blockAt(offset int) (int, bool) {
	if len(d.blocks) == 0 {
		return 0, false
	}
	i := sort.Search(len(d.blocks), func(i int) bool { return d.blocks[i].Span.Start > offset }) - 1
	return max(i, 0), true
}
