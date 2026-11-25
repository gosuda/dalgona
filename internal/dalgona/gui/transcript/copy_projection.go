//go:build gui

package transcript

import (
	"context"
	"io"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

type selectionProjection struct {
	id     document.Identity
	span   spillBounds
	input  sheetInput
	fields []logicalField
	bytes  int
}

func (t *Transcript) captureOrdinaryRow(p *pane, r *row, span spillBounds) (logicalField, error) {
	if completeSelectionRow(r) {
		text, err := selectionText(r.doc, span)
		return logicalField{inline: text}, err
	}
	if r.synth != synthNone || r.entry.Kind == KindGroup {
		s := t.sheetFor(p, r)
		text, err := projectSelection(r.id, s, span)
		return logicalField{inline: text}, err
	}
	selection, err := captureProjection(p, r, span)
	if err != nil {
		return logicalField{}, err
	}
	if field, ok := selection.literal(); ok {
		return r.captureLiteral(field, span, selection)
	}
	return logicalField{projection: &selection}, nil
}

func (r *row) captureLiteral(f logicalField, span spillBounds, selection selectionProjection) (logicalField, error) {
	if !span.partialStart && !span.partialEnd {
		return f, nil
	}
	if r.doc != nil && !r.sourceEvicted && f.live.size == 0 {
		for _, b := range r.sheet.sources {
			if b.ref == f.ref && b.complete() {
				return r.spillField(b, span, false)
			}
		}
	}
	return logicalField{projection: &selection}, nil
}

func completeSelectionRow(r *row) bool {
	if r.doc == nil || r.sourceEvicted || r.sheet.incomplete() {
		return false
	}
	return selectionBodiesComplete(r.sheet.sources, r.pres.Mode == ModeTail && r.sheet.note != "")
}

func selectionBodiesComplete(sources []*body, cropped bool) bool {
	for _, b := range sources {
		if b.start != 0 || !b.complete() || cropped && !b.large() && !b.full {
			return false
		}
	}
	return true
}

func captureProjection(p *pane, r *row, span spillBounds) (selectionProjection, error) {
	e := r.entry
	expanded, set := p.expanded[r.id]
	if !set {
		expanded = e.Kind == KindThinking && e.Live()
	}
	projection := selectionProjection{id: r.id, span: span, input: sheetInput{
		entry: e, pres: r.pres, expanded: expanded,
		diffSplit: r.diffActions.split, diffFull: r.diffActions.full,
	}}
	projection.input.entry.Item.Tail = nil
	projection.input.entry.Item.Changes = nil
	if err := projection.captureDisplayFields(r.sheet.sources); err != nil {
		return selectionProjection{}, err
	}
	if e.Item.Field != "" && len(e.Item.Tail) != 0 {
		if err := projection.captureField(e, e.Item.Field); err != nil {
			return selectionProjection{}, err
		}
	}
	return projection, nil
}

func (p *selectionProjection) captureDisplayFields(sources []*body) error {
	e := p.input.entry
	if e.Kind == KindTool && p.input.expanded && p.input.pres.Mode == ModeFull {
		if err := p.captureField(e, "arguments"); err != nil {
			return err
		}
	}
	for _, b := range sources {
		ref, ok := refNamed(e.Item.Header, b.ref.Field)
		if !ok || ref != b.ref {
			return errRecordedSourceChanged
		}
		if err := p.captureField(e, b.ref.Field); err != nil {
			return err
		}
	}
	return nil
}

func (p *selectionProjection) captureField(e Entry, name string) error {
	for _, field := range p.fields {
		if field.ref.Field == name {
			return nil
		}
	}
	f := viewField{thread: e.Thread, item: e.Item.Key}
	f.bindSource(e.Item, fieldSpec{name: name})
	if f.err != nil {
		return f.err
	}
	if f.total > document.MaxCopyBytes {
		return errSourceBudget
	}
	if !f.hasRef {
		return nil
	}
	ref := f.ref
	if ref.ContentID == "" {
		ref.Field = name
	}
	n := int(f.total) //nolint:gosec // the clipboard limit bounds the captured size.
	if n > document.MaxCopyBytes-p.bytes {
		return errSourceBudget
	}
	p.fields = append(p.fields, logicalField{thread: f.thread, item: f.item, ref: ref, live: f.live, bytes: n, recorded: f.recordedBytes()})
	p.bytes += n
	return nil
}

func (p selectionProjection) literal() (logicalField, bool) {
	if len(p.fields) != 1 {
		return logicalField{}, false
	}
	f := p.fields[0]
	switch p.input.entry.Kind {
	case KindCommand, KindUser, KindPlanItem:
		return f, true
	case KindAssistant:
		return f, f.bytes > document.MaxMarkdownBytes
	case KindThinking:
		return f, p.input.pres.Mode == ModeFull && p.input.expanded
	case KindTool, KindAdvisor, KindFileChange, KindCompaction, KindOther, KindGroup:
	}
	return logicalField{}, false
}

func (p selectionProjection) write(ctx context.Context, r *reviewer, out io.Writer) error {
	const rowBackings = 4
	if p.bytes > residentBudget/rowBackings {
		return errSourceBudget
	}
	replay := newLoader()
	replay.projectionOwner = r.t.loader
	if p.bytes != 0 && !replay.holdProjection(p.bytes*rowBackings) {
		return errSourceBudget
	}
	defer replay.releaseProjections()
	s, err := p.rebuild(ctx, r, replay)
	if err != nil {
		return err
	}
	text, err := projectSelection(p.id, s, p.span)
	if err != nil {
		return err
	}
	_, err = io.WriteString(out, text)
	return err
}

func (p selectionProjection) rebuild(ctx context.Context, r *reviewer, l *loader) (sheet, error) {
	in := p.input
	in.read = reader{loader: l, thread: in.entry.Thread, item: in.entry.Item}
	for _, f := range p.fields {
		if err := loadSelectionField(ctx, r, &in.read, f); err != nil {
			return sheet{}, err
		}
	}
	s := buildSheet(in)
	if s.projectionErr != nil {
		return sheet{}, s.projectionErr
	}
	if s.wait {
		return sheet{}, errStillLoading
	}
	return s, nil
}

func loadSelectionField(ctx context.Context, r *reviewer, read *reader, f logicalField) error {
	src := reviewSourceReader{size: f.bytes, page: func(offset, limit int) (string, error) {
		return r.readLogical(ctx, f, offset, limit)
	}}
	data := make([]byte, f.bytes)
	if _, err := io.ReadFull(&src, data); err != nil {
		return err
	}
	ref := f.ref
	total := uint64(len(data))
	if ref.ContentID == "" {
		ref.ContentID = itemField + ref.Field
		ref.Total = rpc.FormatDecimal(total)
		read.item.Header.Contents = append(append([]rpc.ContentReference(nil), read.item.Header.Contents...), ref)
	}
	b := read.loader.wantRef(f.thread, f.item, ref, true)
	b.data, b.next, b.total = data, total, total
	if b.large() {
		c := read.loader.counter(f.thread, f.item, ref)
		c.next = c.end()
		c.countPage(data[:c.next])
	}
	return nil
}

func projectSelection(id document.Identity, s sheet, span spillBounds) (string, error) {
	doc, err := document.New(document.Header{Identity: id, Format: s.format, Open: s.open, PatchProjection: s.patchProjection}, clean(s.source))
	if err != nil {
		return "", err
	}
	return selectionText(doc, span)
}

func selectionText(doc *document.Document, span spillBounds) (string, error) {
	if doc.Len() == 0 {
		return "", nil
	}
	from, to := span.from, span.to
	if !span.partialEnd {
		to.Offset = len(doc.Source())
	}
	return doc.Text(from, to)
}
