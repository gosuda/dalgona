//go:build gui

package transcript

import (
	"image"
	"sort"
	"strconv"
	"strings"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

type textObject struct {
	node    platform.SemanticNode
	span    document.Span
	link    document.Link
	parent  platform.NodeID
	plain   bool
	actions access.Actions
}

type accessibleRow struct {
	doc    *document.Document
	block  int
	parts  []textObject
	id     platform.NodeID
	parent platform.NodeID
}

func (t *Transcript) semanticPane(gtx layout.Context, env shell.Env, p *pane, rows []*row) {
	id := platform.NodeID(access.Path("transcript", string(env.Thread.Endpoint), env.Thread.Thread))
	n := platform.SemanticNode{ID: id, Role: platform.RoleDocument, Name: "Transcript", TextContainer: true, TextSelectable: true}
	access.Group(gtx, n, access.Actions{Tag: &t.focus}, func(gtx layout.Context) layout.Dimensions {
		t.layoutRows(gtx, env, p, rows)
		return layout.Dimensions{Size: gtx.Constraints.Max}
	})
}

type accessibleList struct {
	list   platform.NodeID
	item   platform.NodeID
	parent platform.NodeID
	marker byte
}

type accessRows struct {
	lists    []accessibleList
	id       string
	entry    int
	block    int
	previous accessTextEdge
}

func (t *Transcript) accessRows(p *pane) {
	clear(p.accessible)
	p.accessible = p.accessible[:0]
	state := accessRows{entry: -1, block: -1}
	for i := range p.vp.Rows() {
		ref := p.vp.Row(i)
		r := p.order[ref.Entry]
		if ref.Entry != state.entry {
			state.id = access.Path(r.id.Turn, r.id.Item, r.id.Draft)
		}
		described := state.describe(r, ref, state.id)
		t.bindRowAccess(p, i, &described, &state.previous)
		p.accessible = append(p.accessible, described)
	}
}

func (s *accessRows) describe(r *row, ref document.RowRef, id string) accessibleRow {
	if ref.Block < 0 || ref.Block >= r.doc.Len() {
		return accessibleRow{block: -1}
	}
	b := r.doc.Block(ref.Block)
	out := accessibleRow{doc: r.doc, block: ref.Block}
	parent := s.enter(&out, b, ref, id)
	blockID := id + "/" + strconv.Itoa(b.Key.Span.Start)
	rowID := blockID + "/" + strconv.Itoa(ref.Row)
	out.id, out.parent = platform.NodeID(rowID), parent
	span := b.Rows[ref.Row]
	switch b.Kind {
	case document.KindTable:
		out.table(b, ref.Row, platform.NodeID(blockID), platform.NodeID(rowID), parent)
	case document.KindFigure, document.KindMath:
		if ref.Row == 0 {
			out.group(platform.NodeID(blockID), parent, blockRole(b.Kind), "")
		}
		span.Start = min(span.End, max(span.Start, b.Body.Start))
		out.text(b, span, rowID, platform.NodeID(blockID))
	case document.KindParagraph, document.KindHeading, document.KindCode, document.KindRule, document.KindHTML, document.KindNotice, document.KindDiff:
		if ref.Row == 0 && b.Task != document.TaskNone {
			out.checkbox(b, blockID, parent)
		}
		out.text(b, span, rowID, parent)
	}
	return out
}

func (r *accessibleRow) checkbox(b document.Block, id string, parent platform.NodeID) {
	state := "unchecked"
	checked := b.Task == document.TaskDone
	if checked {
		state = "checked"
	}
	r.parts = append(r.parts, textObject{node: platform.SemanticNode{
		ID:          platform.NodeID(id + "/task"),
		Role:        platform.RoleCheckBox,
		Name:        "Task",
		Description: "Task " + state,
		Checked:     checked,
		Disabled:    true,
	}, parent: parent, plain: true})
}

func blockRole(kind document.Kind) platform.Role {
	if kind == document.KindMath {
		return platform.RoleMath
	}
	return platform.RoleFigure
}

func (s *accessRows) enter(out *accessibleRow, b document.Block, ref document.RowRef, id string) platform.NodeID {
	if s.entry == ref.Entry && s.block == ref.Block {
		return s.parent()
	}
	if s.entry != ref.Entry {
		s.lists = s.lists[:0]
	}
	s.entry, s.block = ref.Entry, ref.Block
	return s.listItems(out, b, id)
}

func (s *accessRows) parent() platform.NodeID {
	if len(s.lists) != 0 {
		return s.lists[len(s.lists)-1].item
	}
	return ""
}

func (s *accessRows) listItems(out *accessibleRow, b document.Block, id string) platform.NodeID {
	var parent platform.NodeID
	depth := 0
	for _, c := range b.Containers {
		if c.Kind != document.ContainerListItem {
			continue
		}
		path := id + "/list/" + strconv.Itoa(b.Key.Span.Start) + "/" + strconv.Itoa(depth)
		s.listItem(out, c, depth, platform.NodeID(path), parent)
		parent = s.lists[depth].item
		depth++
	}
	s.lists = s.lists[:depth]
	return parent
}

func (s *accessRows) listItem(out *accessibleRow, c document.Container, depth int, id, parent platform.NodeID) {
	fresh := depth == len(s.lists)
	if fresh {
		s.lists = append(s.lists, accessibleList{})
	}
	level := &s.lists[depth]
	if level.starts(c, parent) {
		*level = accessibleList{list: id, parent: parent, marker: c.Marker}
		name := "List"
		if c.Ordered {
			name = "Ordered list"
		}
		out.group(id, parent, platform.RoleList, name)
	}
	if level.item == "" || c.First {
		level.item = id + "/item"
		out.group(level.item, level.list, platform.RoleListItem, "")
	}
}

func (level accessibleList) starts(c document.Container, parent platform.NodeID) bool {
	return level.list == "" || level.parent != parent || level.marker != c.Marker || c.First && c.Number == c.Start
}

func (r *accessibleRow) group(id, parent platform.NodeID, role platform.Role, name string) {
	r.parts = append(r.parts, textObject{node: platform.SemanticNode{ID: id, Role: role, Name: name}, parent: parent, plain: true})
}

func (r *accessibleRow) text(b document.Block, span document.Span, id string, parent platform.NodeID) {
	for _, part := range platform.TextObjects(b.Projection.Text[span.Start:span.End]) {
		chunk := document.Span{Start: span.Start + part.Start, End: span.Start + part.End}
		n := textNode(b, id+"/"+strconv.Itoa(part.Start), b.Projection.Text[chunk.Start:chunk.End])
		r.parts = append(r.parts, textObject{node: n, span: chunk, parent: parent})
	}
	r.parts = append(r.parts, describeLinks(b, span, id, parent)...)
}

func (r *accessibleRow) table(b document.Block, row int, tableID, rowID, parent platform.NodeID) {
	if row == 0 {
		r.group(tableID, parent, platform.RoleTable, "Table")
	}
	r.group(rowID, tableID, platform.RoleRow, "")
	role := platform.RoleCell
	if row == 0 {
		role = platform.RoleColumnHeader
	}
	for column, span := range tableColumns(b, row) {
		id := rowID + "/cell/" + platform.NodeID(strconv.Itoa(column))
		r.tableCell(b, id, rowID, role, span)
	}
}

func (r *accessibleRow) tableCell(b document.Block, id, parent platform.NodeID, role platform.Role, span document.Span) {
	value := b.Projection.Text[span.Start:span.End]
	n := platform.SemanticNode{ID: id, Role: role, RowSpan: 1, ColumnSpan: 1, Name: boundedName(value)}
	if len(value) > platform.TextObjectLimit {
		r.parts = append(r.parts, textObject{node: n, parent: parent, plain: true})
		r.text(b, span, string(id), id)
		return
	}
	n.Value = value
	r.parts = append(r.parts, textObject{node: n, span: span, parent: parent})
	r.parts = append(r.parts, describeLinks(b, span, string(id), id)...)
}

func tableColumns(b document.Block, row int) []document.Span {
	span := b.Rows[row]
	text := b.Projection.Text
	if span.Len() > 0 && text[span.End-1] == '\n' {
		span.End--
	}
	start := span.Start
	columns := make([]document.Span, 0, len(b.Alignments))
	runs := b.Projection.Runs
	at := sort.Search(len(runs), func(i int) bool { return runs[i].Text.End > start })
	for _, run := range runs[at:] {
		if run.Text.Start >= span.End {
			break
		}
		if (run.Kind != document.Break && run.Kind != document.Generated) || text[run.Text.Start:run.Text.End] != "\t" {
			continue
		}
		columns = append(columns, document.Span{Start: start, End: run.Text.Start})
		start = run.Text.End
	}
	return append(columns, document.Span{Start: start, End: span.End})
}

func boundedName(value string) string {
	name := firstLine(value)
	if len(name) > platform.TextObjectLimit {
		name = name[:platform.TextObjects(name)[0].End]
	}
	return name
}

func textNode(b document.Block, id, value string) platform.SemanticNode {
	n := platform.SemanticNode{ID: platform.NodeID(id), Role: platform.RoleDocument, Value: value}
	switch b.Kind {
	case document.KindHeading:
		n.Role, n.Level, n.Name = platform.RoleHeading, b.Level, strings.TrimSpace(value)
	case document.KindCode, document.KindDiff:
		n.Role, n.Name, n.Description = platform.RoleCode, "Code", b.Language
	case document.KindMath, document.KindFigure:
		n.Name = "Source"
	case document.KindParagraph, document.KindTable, document.KindRule, document.KindHTML, document.KindNotice:
	}
	return n
}

func describeLinks(b document.Block, span document.Span, id string, parent platform.NodeID) []textObject {
	if len(b.Links) == 0 {
		return nil
	}
	runs := b.Projection.Runs
	at := sort.Search(len(runs), func(i int) bool { return runs[i].Text.End > span.Start })
	var links []textObject
	for k := at; k < len(runs) && runs[k].Text.Start < span.End; k++ {
		run := runs[k]
		if run.Link <= 0 || run.Link > len(b.Links) {
			continue
		}
		part := document.Span{Start: max(span.Start, run.Text.Start), End: min(span.End, run.Text.End)}
		link := b.Links[run.Link-1]
		name := b.Projection.Text[part.Start:part.End]
		n := platform.SemanticNode{ID: platform.NodeID(id + "/link/" + strconv.Itoa(part.Start)), Role: platform.RoleLink, Name: name, Description: link.Destination, Action: "open"}
		links = append(links, textObject{node: n, span: part, link: link, parent: parent})
	}
	return links
}

func (t *Transcript) registerRows(gtx layout.Context, p *pane, done map[int]laid) {
	if !access.Enabled(gtx) {
		return
	}
	if len(p.accessible) != p.vp.Rows() {
		t.accessRows(p)
	}
	first, last := p.vp.Window()
	for i, row := range p.accessible {
		info := placed{}
		if l, ok := done[i]; ok && first <= i && i < last {
			info = l.info
			info.y = int((p.vp.RowTop(i) - p.vp.Offset()) * float64(p.pxPerDp))
		}
		t.registerEntryRow(gtx, p, i, row, info)
	}
}

func (t *Transcript) registerEntryRow(gtx layout.Context, p *pane, i int, row accessibleRow, info placed) {
	ref := p.vp.Row(i)
	r := p.order[ref.Entry]
	if i == 0 || p.vp.Row(i-1).Entry != ref.Entry {
		t.registerContent(gtx, p, i, r)
	}
	t.registerRow(gtx, p, i, row, info)
	if i+1 == p.vp.Rows() || p.vp.Row(i+1).Entry != ref.Entry {
		t.registerImages(gtx, p, i, r)
	}
}

func (t *Transcript) registerRow(gtx layout.Context, p *pane, index int, row accessibleRow, info placed) {
	at := info.text.Add(image.Pt(0, info.y))
	size := info.size()
	if !size.Eq(image.Point{}) {
		defer op.Offset(at).Push(gtx.Ops).Pop()
	}
	for _, object := range row.parts {
		n := p.rowAccess(gtx, index, row, object)
		a := object.actions
		if n.Role == platform.RoleFigure || n.Role == platform.RoleMath {
			t.registerSpecial(gtx, row.doc.Block(row.block), n, a)
			continue
		}
		registerRowObject(gtx, n, object, info, size)
	}
	if row.doc != nil {
		t.registerInlineMath(gtx, row.doc.Block(row.block), p.vp.Row(index), row.id, row.parent)
	}
}

func registerRowObject(gtx layout.Context, n platform.SemanticNode, object textObject, info placed, size image.Point) {
	if size.Eq(image.Point{}) || object.plain {
		access.Offscreen(gtx, n, object.actions)
		return
	}
	bounds := image.Rectangle{Max: size}
	switch {
	case n.Role == platform.RoleLink:
		bounds = linkBounds(info.layout, object.span)
	case info.split != nil:
		n.Lines = info.split.semanticLines(object.span)
	default:
		n.Lines = info.layout.SemanticLines(object.span)
	}
	access.Add(gtx, bounds, n, object.actions)
}

func (t *Transcript) bindRowAccess(p *pane, index int, row *accessibleRow, previous *accessTextEdge) {
	scroll := func() { p.scrolled = p.vp.RowTop(index) - p.vp.Offset() }
	for i := range row.parts {
		object := &row.parts[i]
		a := access.Actions{Scroll: scroll}
		switch {
		case object.node.Role == platform.RoleLink:
			t.bindLinkAccess(p, object, &a)
		case row.doc != nil && !object.plain:
			a.Selection = p.sel
			a.TextPosition = p.textPosition(row.doc, row.block, object.span.Start)
			start := accessTextEdge{doc: row.doc, point: document.ProjectionPoint{Block: row.block, Byte: object.span.Start}}
			object.node.TextBefore = previous.between(start)
			*previous = accessTextEdge{doc: row.doc, point: document.ProjectionPoint{Block: row.block, Byte: object.span.End}}
		}
		object.actions = access.Within(object.parent, a)
	}
}

func (t *Transcript) bindLinkAccess(p *pane, object *textObject, actions *access.Actions) {
	link := object.link
	target, err := link.Target()
	object.node.Disabled = err != nil || t.deps.OpenLink == nil
	if object.node.Disabled {
		return
	}
	actions.ActivationKey = target
	actions.Activate = func() {
		if !t.env.Modal && (!t.review.active() || p == t.review.pane) {
			t.openLink(link)
		}
	}
}

func (p *pane) textPosition(doc *document.Document, block, spanStart int) func(int, document.Affinity) (document.Position, error) {
	id, revision := doc.Identity(), doc.Revision()
	fingerprint := doc.Block(block).Fingerprint
	return func(offset int, affinity document.Affinity) (document.Position, error) {
		entry, ok := p.scope.Index(id)
		if !ok {
			return document.Position{}, document.ErrNoPosition
		}
		current, ok := p.scope.Loaded(entry)
		if !ok || current.Revision() != revision || block >= current.Len() || current.Block(block).Fingerprint != fingerprint {
			return document.Position{}, document.ErrNoPosition
		}
		return current.At(block, spanStart+offset, affinity)
	}
}

func (p *pane) rowAccess(gtx layout.Context, index int, row accessibleRow, object textObject) platform.SemanticNode {
	n := object.node
	if object.actions.TextPosition != nil {
		n.TextGroup = access.Parent(gtx)
		selected := p.selectedSpan(p.order[p.vp.Row(index).Entry], row.block, object.span)
		if selected.Len() > 0 {
			n.Selection = inputtext.Range{Start: selected.Start - object.span.Start, End: selected.End - object.span.Start}
		}
	}
	return n
}

type accessTextEdge struct {
	doc   *document.Document
	point document.ProjectionPoint
}

func (e accessTextEdge) between(to accessTextEdge) string {
	if e.doc == nil {
		return ""
	}
	if e.doc == to.doc {
		return e.doc.ProjectedText(e.point, to.point)
	}
	last := e.doc.Len() - 1
	end := document.ProjectionPoint{Block: last, Byte: len(e.doc.Block(last).Projection.Text)}
	return e.doc.ProjectedText(e.point, end) + "\n\n" + to.doc.ProjectedText(document.ProjectionPoint{}, to.point)
}

func linkBounds(rl *document.RowLayout, span document.Span) image.Rectangle {
	var bounds image.Rectangle
	for _, b := range rl.Highlight(span) {
		bounds = bounds.Union(image.Rect(int(b.X0+rl.Inset), int(b.Y0), int(b.X1+rl.Inset+0.999), int(b.Y1+0.999)))
	}
	return bounds
}
