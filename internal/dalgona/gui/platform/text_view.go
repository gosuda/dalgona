package platform

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type TextPoint struct {
	Node NodeID
	Byte int
	End  bool
}

type TextRange struct {
	Start, End TextPoint
}

func (t *AccessibilityTree) QueryTextRange(s *SemanticSnapshot, r TextRange) (string, error) {
	if s == nil {
		return "", ErrElementUnavailable
	}
	n, ok := s.Node(r.Start.Node)
	if !ok || n.TextGroup == "" {
		return "", ErrElementUnavailable
	}
	return t.textView(s, n.TextGroup).read(r)
}

func (t *AccessibilityTree) SelectTextRange(s *SemanticSnapshot, r TextRange) error {
	if s == nil {
		return ErrElementUnavailable
	}
	n, ok := s.Node(r.Start.Node)
	if !ok || n.TextGroup == "" {
		return ErrElementUnavailable
	}
	return t.textView(s, n.TextGroup).selectRange(r)
}

func (s *SemanticSnapshot) textGroupChanges(before *SemanticSnapshot, id NodeID) (content, selection bool) {
	parts := s.textParts[id]
	content = !slices.Equal(parts, before.textParts[id])
	for _, part := range parts {
		old, n := before.nodes[part], s.nodes[part]
		content = content || old.Value != n.Value || old.TextBefore != n.TextBefore || old.TextState != n.TextState
		selection = selection || old.Selection != n.Selection
	}
	return content, selection
}

func (s *SemanticSnapshot) placeText(n SemanticNode) error {
	if n.TextGroup == "" {
		return nil
	}
	if !s.nodes[n.TextGroup].TextContainer || n.Role == RoleSecureField || n.ID == n.TextGroup {
		return fmt.Errorf("semantic node %q has an invalid text container %q", n.ID, n.TextGroup)
	}
	if len(n.Value) > TextObjectLimit {
		return fmt.Errorf("semantic text object %q exceeds %d bytes", n.ID, TextObjectLimit)
	}
	if s.textParts == nil {
		s.textParts = make(map[NodeID][]NodeID)
		s.textOrder = make(map[NodeID]int)
	}
	s.textOrder[n.ID] = len(s.textParts[n.TextGroup])
	s.textParts[n.TextGroup] = append(s.textParts[n.TextGroup], n.ID)
	return nil
}

type textView struct {
	tree *AccessibilityTree
	snap *SemanticSnapshot
	root NodeID
}

func (t *AccessibilityTree) textView(s *SemanticSnapshot, root NodeID) textView {
	return textView{tree: t, snap: s, root: root}
}

func (v textView) parts() []NodeID { return v.snap.textParts[v.root] }

func (v textView) whole() TextRange {
	parts := v.parts()
	if len(parts) == 0 {
		return TextRange{}
	}
	return TextRange{Start: TextPoint{Node: parts[0]}, End: TextPoint{Node: parts[len(parts)-1], End: true}}
}

func (v textView) part(id NodeID) (SemanticNode, error) {
	n, ok := v.snap.Node(id)
	if !ok || n.TextGroup != v.root {
		return SemanticNode{}, ErrElementUnavailable
	}
	return n, nil
}

func (v textView) load(id NodeID) (SemanticNode, error) {
	n, err := v.part(id)
	if err != nil || n.TextState == TextReady {
		return n, err
	}
	_, err = v.tree.QueryText(id, v.snap.Revision())
	return n, err
}

func (v textView) compare(a, b TextPoint) (int, error) {
	if _, err := v.part(a.Node); err != nil {
		return 0, err
	}
	if _, err := v.part(b.Node); err != nil {
		return 0, err
	}
	if a.Node != b.Node {
		return cmp.Compare(v.snap.textOrder[a.Node], v.snap.textOrder[b.Node]), nil
	}
	if a.End != b.End {
		n, err := v.load(a.Node)
		if err != nil {
			return 0, err
		}
		return cmp.Compare(a.offset(n), b.offset(n)), nil
	}
	if a.End {
		return 0, nil
	}
	return cmp.Compare(a.Byte, b.Byte), nil
}

func (p TextPoint) offset(n SemanticNode) int {
	if p.End {
		return len(n.Value)
	}
	return p.Byte
}

func (v textView) point(pos int, unit inputtext.Unit) (TextPoint, error) {
	return v.locate(pos, unit, false)
}

func (v textView) locate(pos int, unit inputtext.Unit, clampEnd bool) (TextPoint, error) {
	if pos < 0 {
		return TextPoint{}, inputtext.InvalidOffset
	}
	for _, id := range v.parts() {
		n := v.snap.nodes[id]
		prefix := inputtext.Count(n.TextBefore, unit)
		if pos < prefix {
			at, err := inputtext.Convert(n.TextBefore, pos, unit, inputtext.Bytes)
			return TextPoint{Node: id, Byte: at - len(n.TextBefore)}, err
		}
		pos -= prefix
		if pos == 0 {
			return TextPoint{Node: id}, nil
		}
		n, err := v.load(id)
		if err != nil {
			return TextPoint{}, err
		}
		length := inputtext.Count(n.Value, unit)
		if pos <= length {
			at, err := inputtext.Convert(n.Value, pos, unit, inputtext.Bytes)
			return TextPoint{Node: id, Byte: at}, err
		}
		pos -= length
	}
	if clampEnd || pos == 0 {
		return v.whole().End, nil
	}
	return TextPoint{}, inputtext.InvalidOffset
}

func (v textView) read(r TextRange) (string, error) {
	return v.readUTF16(r, -1)
}

func (v textView) readUTF16(r TextRange, maxUnits int) (string, error) {
	if maxUnits == 0 {
		return "", nil
	}
	if r.Start.Node == "" && r.End.Node == "" {
		return "", nil
	}
	if order, err := v.compare(r.Start, r.End); err != nil || order > 0 {
		return "", cmp.Or(err, error(inputtext.InvalidOffset))
	}
	first, last := v.snap.textOrder[r.Start.Node], v.snap.textOrder[r.End.Node]
	if first == last && r.Start.Byte >= 0 && r.End.Byte >= 0 {
		return v.readPart(r, maxUnits)
	}
	return v.join(r, v.parts()[first:last+1], maxUnits)
}

func (v textView) readPart(r TextRange, maxUnits int) (string, error) {
	n, err := v.load(r.Start.Node)
	if err != nil {
		return "", err
	}
	text, err := textSlice(n.Value, r.Start.offset(n), r.End.offset(n))
	if err != nil || maxUnits < 0 {
		return text, err
	}
	prefix, _ := utf16Prefix(text, maxUnits)
	return prefix, nil
}

type textOutput struct {
	text  strings.Builder
	units int
}

func (v textView) join(r TextRange, parts []NodeID, maxUnits int) (string, error) {
	if maxUnits >= TextObjectLimit {
		maxUnits = -1
	}
	out := textOutput{units: maxUnits}
	for _, id := range parts {
		n := v.snap.nodes[id]
		stop, err := v.joinPart(&out, n, r)
		if err != nil {
			return "", err
		}
		if stop {
			break
		}
	}
	return out.text.String(), nil
}

func (v textView) joinPart(out *textOutput, n SemanticNode, r TextRange) (bool, error) {
	if clipped, err := appendBefore(out, n, r); clipped || err != nil {
		return true, err
	}
	if n.ID == r.End.Node && r.End.Byte <= 0 && !r.End.End {
		return true, nil
	}
	return v.appendPart(out, n.ID, r)
}

func appendBefore(out *textOutput, n SemanticNode, r TextRange) (bool, error) {
	from, to := 0, len(n.TextBefore)
	if n.ID == r.Start.Node {
		from = to + min(r.Start.Byte, 0)
	}
	if n.ID == r.End.Node {
		to += min(r.End.Byte, 0)
	}
	return out.append(n.TextBefore, from, to)
}

func (v textView) appendPart(out *textOutput, id NodeID, r TextRange) (bool, error) {
	n, err := v.load(id)
	if err != nil {
		return false, err
	}
	start, end := 0, len(n.Value)
	if id == r.Start.Node {
		start = max(0, r.Start.offset(n))
	}
	if id == r.End.Node {
		end = r.End.offset(n)
	}
	return out.append(n.Value, start, end)
}

func (out *textOutput) append(value string, start, end int) (bool, error) {
	text, err := textSlice(value, start, end)
	if err != nil {
		return false, err
	}
	limit := TextObjectLimit - out.text.Len()
	if len(text) > limit {
		prefix, err := boundedTextPrefix(text, limit)
		if err != nil {
			return false, err
		}
		out.write(prefix)
		return true, nil
	}
	out.write(text)
	return out.units == 0 || out.text.Len() == TextObjectLimit, nil
}

func (out *textOutput) write(text string) {
	if out.units >= 0 {
		text, out.units = utf16Prefix(text, out.units)
	}
	out.text.WriteString(text)
}

func boundedTextPrefix(text string, limit int) (string, error) {
	for !utf8.RuneStart(text[limit]) {
		limit--
	}
	at, err := inputtext.PaintRange(text, inputtext.Range{Start: limit, End: limit})
	return text[:at.Start], err
}

func textSlice(text string, start, end int) (string, error) {
	if start < 0 || end < start || end > len(text) {
		return "", inputtext.InvalidOffset
	}
	if !scalarBoundary(text, start) || !scalarBoundary(text, end) {
		return "", inputtext.InvalidOffset
	}
	return text[start:end], nil
}

func scalarBoundary(text string, at int) bool {
	return at == len(text) || utf8.RuneStart(text[at])
}

func (v textView) selectRange(r TextRange) error {
	if !v.snap.nodes[v.root].TextSelectable {
		return ErrElementUnavailable
	}
	order, err := v.compare(r.Start, r.End)
	if err != nil || order > 0 {
		return cmp.Or(err, error(inputtext.InvalidOffset))
	}
	start, err := v.selectionPoint(r.Start, true)
	if err != nil {
		return err
	}
	end := start
	if order != 0 {
		end, err = v.selectionPoint(r.End, false)
		if err != nil {
			return err
		}
	}
	return v.tree.Request(AccessibilityIntent{Node: start.Node, Revision: v.snap.Revision(), Action: ActionSelectText, Selection: inputtext.Range{Start: start.Byte, End: end.Byte}, SelectionEnd: end.Node})
}

func (v textView) selectionPoint(p TextPoint, start bool) (TextPoint, error) {
	if p.Byte < 0 && start {
		if previous := v.snap.textOrder[p.Node] - 1; previous >= 0 {
			p = TextPoint{Node: v.parts()[previous], End: true}
		}
	}
	n, err := v.load(p.Node)
	if err != nil {
		return TextPoint{}, err
	}
	at := max(0, p.offset(n))
	if err := inputtext.CheckRange(n.Value, inputtext.Range{Start: at, End: at}); err != nil {
		return TextPoint{}, err
	}
	return TextPoint{Node: p.Node, Byte: at}, nil
}

func (v textView) selectUnits(start, end int, unit inputtext.Unit) error {
	r, err := v.unitsRange(start, end, unit)
	if err != nil {
		return err
	}
	return v.tree.SelectTextRange(v.snap, r)
}

func (v textView) unitsRange(start, end int, unit inputtext.Unit) (TextRange, error) {
	if start > end {
		return TextRange{}, inputtext.InvalidOffset
	}
	from, err := v.point(start, unit)
	if err != nil {
		return TextRange{}, err
	}
	to, err := v.point(end, unit)
	return TextRange{Start: from, End: to}, err
}
