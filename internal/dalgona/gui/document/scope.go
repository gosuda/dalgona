package document

import (
	"context"
	"errors"
	"fmt"
	"strings"
)

var (
	ErrCopyLimit = errors.New("selection is too large to copy")
	ErrPage      = errors.New("document source page could not be read")
	ErrUnknown   = errors.New("document is not in this scope")
)

type Ref struct {
	Header   Header
	Revision Revision
	Length   int
}

type Pager interface {
	Page(ctx context.Context, ref Ref, offset, limit int) (string, error)
}

type streamSpan struct {
	from int
	to   int
}

type Scope struct {
	endpoint string
	thread   string
	entries  []entry
	index    map[Identity]int
	clock    uint64
	folds    map[Key]bool
}

type entry struct {
	ref  Ref
	doc  *Document
	used uint64
}

func NewScope(endpoint, thread string) *Scope {
	return &Scope{endpoint: endpoint, thread: thread, index: map[Identity]int{}}
}

func (s *Scope) Len() int                       { return len(s.entries) }
func (s *Scope) Ref(i int) Ref                  { return s.entries[i].ref }
func (s *Scope) Loaded(i int) (*Document, bool) { return s.entries[i].doc, s.entries[i].doc != nil }

func (s *Scope) Index(id Identity) (int, bool) {
	i, ok := s.index[id]
	return i, ok
}

func (s *Scope) owns(id Identity) bool {
	if id.Endpoint != s.endpoint {
		return false
	}
	return id.Thread == s.thread || (id.IsDraft() && id.Thread == "")
}

func refOf(d *Document) Ref {
	return Ref{Header: d.header, Revision: d.revision, Length: len(d.source)}
}

func (s *Scope) Put(d *Document) (int, error) {
	if !s.owns(d.header.Identity) {
		return 0, fmt.Errorf("%w: %+v belongs to another endpoint or thread", ErrForeign, d.header.Identity)
	}
	if i, ok := s.index[d.header.Identity]; ok {
		if d.revision < s.entries[i].ref.Revision {
			return i, fmt.Errorf("%w: revision %d is older than %d", ErrStale, d.revision, s.entries[i].ref.Revision)
		}
		s.clock++
		s.entries[i] = entry{ref: refOf(d), doc: d, used: s.clock}
		return i, nil
	}
	s.clock++
	s.entries = append(s.entries, entry{ref: refOf(d), doc: d, used: s.clock})
	s.index[d.header.Identity] = len(s.entries) - 1
	return len(s.entries) - 1, nil
}

func (s *Scope) Prepend(refs ...Ref) error {
	for _, ref := range refs {
		if !s.owns(ref.Header.Identity) {
			return fmt.Errorf("%w: %+v belongs to another endpoint or thread", ErrForeign, ref.Header.Identity)
		}
		if _, ok := s.index[ref.Header.Identity]; ok {
			return fmt.Errorf("%w: %+v is already in this scope", ErrIdentity, ref.Header.Identity)
		}
	}
	entries := make([]entry, 0, len(refs)+len(s.entries))
	for _, ref := range refs {
		entries = append(entries, entry{ref: ref})
	}
	s.entries = append(entries, s.entries...)
	s.reindex()
	return nil
}

func (s *Scope) reindex() {
	s.index = make(map[Identity]int, len(s.entries))
	for i, e := range s.entries {
		s.index[e.ref.Header.Identity] = i
	}
}

func (s *Scope) Unload(i int) {
	s.entries[i].doc = nil
}

func (s *Scope) Resident() int {
	total := 0
	for _, e := range s.entries {
		if e.doc != nil {
			total += len(e.doc.source)
		}
	}
	return total
}

func (s *Scope) Evict(budget int, pinned func(Identity) bool) {
	for resident := s.Resident(); resident > budget; {
		victim := -1
		for i, e := range s.entries {
			if e.doc != nil && !pinned(e.ref.Header.Identity) && (victim < 0 || e.used < s.entries[victim].used) {
				victim = i
			}
		}
		if victim < 0 {
			return
		}
		resident -= len(s.entries[victim].doc.source)
		s.Unload(victim)
	}
}

func (s *Scope) Edge(i int, end bool) Position {
	ref := s.entries[i].ref
	if end {
		return Position{Document: ref.Header.Identity, Offset: ref.Length, Affinity: Before}
	}
	return Position{Document: ref.Header.Identity, Offset: 0, Affinity: After}
}

func (s *Scope) Load(ctx context.Context, i int, pager Pager) (*Document, error) {
	if doc := s.entries[i].doc; doc != nil {
		s.clock++
		s.entries[i].used = s.clock
		return doc, nil
	}
	ref := s.entries[i].ref
	var source strings.Builder
	source.Grow(ref.Length)
	if err := s.stream(ctx, i, streamSpan{from: 0, to: ref.Length}, pager, func(page string) error {
		source.WriteString(page)
		return nil
	}); err != nil {
		return nil, err
	}
	doc, err := New(ref.Header, source.String())
	if err != nil {
		return nil, fmt.Errorf("%w: %+v: %w", ErrPage, ref.Header.Identity, err)
	}
	doc.revision = ref.Revision
	return doc, nil
}

func (s *Scope) stream(ctx context.Context, i int, span streamSpan, pager Pager, write func(string) error) error {
	ref := s.entries[i].ref
	if doc := s.entries[i].doc; doc != nil {
		return write(doc.source[span.from:span.to])
	}
	for offset := span.from; offset < span.to; {
		if err := ctx.Err(); err != nil {
			return err
		}
		limit := min(PageBytes, span.to-offset)
		page, err := pager.Page(ctx, ref, offset, limit)
		if err != nil {
			return fmt.Errorf("%w: %+v at byte %d: %w", ErrPage, ref.Header.Identity, offset, err)
		}
		if page == "" || len(page) > limit {
			return fmt.Errorf("%w: %+v at byte %d returned %d bytes for a %d byte request", ErrPage, ref.Header.Identity, offset, len(page), limit)
		}
		if err := write(page); err != nil {
			return err
		}
		offset += len(page)
	}
	return nil
}

type bound struct {
	entry    int
	position Position
}

func (s *Scope) ordered(sel Selection) (bound, bound, error) {
	a, ok := s.index[sel.Anchor.Document]
	if !ok {
		return bound{}, bound{}, fmt.Errorf("%w: %+v", ErrUnknown, sel.Anchor.Document)
	}
	f, ok := s.index[sel.Focus.Document]
	if !ok {
		return bound{}, bound{}, fmt.Errorf("%w: %+v", ErrUnknown, sel.Focus.Document)
	}
	first, last := bound{entry: a, position: sel.Anchor}, bound{entry: f, position: sel.Focus}
	if f < a || (f == a && sel.Focus.Offset < sel.Anchor.Offset) {
		first, last = last, first
	}
	return first, last, nil
}

func (s *Scope) Copy(ctx context.Context, sel Selection, pager Pager) (string, error) {
	first, last, err := s.ordered(sel)
	if err != nil {
		return "", err
	}
	var out strings.Builder
	for i := first.entry; i <= last.entry; i++ {
		if s.entries[i].ref.Length > MaxCopyBytes-out.Len() && s.entries[i].doc == nil {
			return "", fmt.Errorf("%w: it exceeds %d bytes; use Save Markdown instead", ErrCopyLimit, MaxCopyBytes)
		}
		text, err := s.copyEntry(ctx, i, first, last, pager)
		if err != nil {
			return "", err
		}
		if i > first.entry {
			out.WriteString("\n\n")
		}
		if out.Len()+len(text) > MaxCopyBytes {
			return "", fmt.Errorf("%w: it exceeds %d bytes; use Save Markdown instead", ErrCopyLimit, MaxCopyBytes)
		}
		out.WriteString(text)
	}
	return out.String(), nil
}

func (s *Scope) copyEntry(ctx context.Context, i int, first, last bound, pager Pager) (string, error) {
	doc, err := s.Load(ctx, i, pager)
	if err != nil {
		return "", err
	}
	if doc.Len() == 0 {
		return "", nil
	}
	from, to := s.Edge(i, false), s.Edge(i, true)
	if i == first.entry {
		from = first.position
	}
	if i == last.entry {
		to = last.position
	}
	from.Document, to.Document = doc.header.Identity, doc.header.Identity
	return doc.Text(from, to)
}

type Matches struct {
	Documents int
	Count     int
	Results   []Selection
}

func (s *Scope) Find(ctx context.Context, query string, pager Pager) (Matches, error) {
	matches := Matches{Documents: len(s.entries)}
	for i := range s.entries {
		doc, err := s.Load(ctx, i, pager)
		if err != nil {
			return Matches{}, err
		}
		found, count := doc.Find(query, MaxFindResults-len(matches.Results))
		matches.Results = append(matches.Results, found...)
		matches.Count += count
	}
	return matches, nil
}
