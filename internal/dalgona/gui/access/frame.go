//go:build gui

package access

import (
	"fmt"
	"image"
	"net/url"
	"reflect"
	"strconv"
	"strings"

	"gioui.org/io/event"
	"gioui.org/io/input"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const (
	contextKey   = "dalgona.access.frame"
	boundsMarker = "\x00dalgona.access:"
	root         = "window"
)

type Actions struct {
	Tag            event.Tag
	Click          *widget.Clickable
	Editor         *platform.Editor
	Activate       func()
	ActivationKey  string
	Scroll         func()
	TextPosition   func(int, document.Affinity) (document.Position, error)
	Selection      *document.State
	Rejected       func(error)
	Address        func(platform.AccessibilityAction) bool
	parent         platform.NodeID
	bounds         image.Rectangle
	bounded        bool
	textSince      uint64
	editorRevision uint64
}

type registered struct {
	node      platform.SemanticNode
	actions   Actions
	placed    bool
	offscreen bool
}

type Frame struct {
	values          map[string]any
	geometry        input.Router
	markers         []input.SemanticNode
	placementTags   []event.Tag
	placements      map[event.Tag]image.Rectangle
	entries         []registered
	previous, next  []platform.SemanticNode
	active, actions map[platform.NodeID]Actions
	scope, parent   platform.NodeID
	bounds          image.Rectangle
	scroll          func()
	resume          []platform.AccessibilityIntent
	logicalFocus    platform.NodeID
}

func (f *Frame) Begin(gtx layout.Context, tree *platform.AccessibilityTree, bounds image.Rectangle) layout.Context {
	f.dispatch(gtx, tree)
	if f.values == nil {
		f.values = map[string]any{contextKey: f}
		f.active = make(map[platform.NodeID]Actions)
		f.actions = make(map[platform.NodeID]Actions)
	}
	clear(f.placementTags)
	f.placementTags = f.placementTags[:0]
	clear(f.entries)
	f.resume = f.resume[:0]
	f.bounds = bounds
	f.scope, f.parent = root, root
	f.entries = append(f.entries[:0], registered{node: platform.SemanticNode{ID: root, Role: platform.RoleDocument, Name: "dalgona", Bounds: bounds}, placed: true})
	gtx.Values = f.values
	return gtx
}

func Enabled(gtx layout.Context) bool {
	return frameFor(gtx) != nil
}

func Hide(gtx layout.Context) layout.Context {
	gtx.Values = nil
	return gtx
}

func frameFor(gtx layout.Context) *Frame {
	f, _ := gtx.Values[contextKey].(*Frame)
	return f
}

func Path(parts ...string) string {
	var b strings.Builder
	for i, part := range parts {
		if i != 0 {
			b.WriteByte('/')
		}
		b.WriteString(url.PathEscape(part))
	}
	return b.String()
}

func ID(gtx layout.Context, local string) platform.NodeID {
	if f := frameFor(gtx); f != nil {
		return f.scope + "/" + platform.NodeID(local)
	}
	return ""
}

func Scope(gtx layout.Context, path string, w layout.Widget) layout.Dimensions {
	f := frameFor(gtx)
	if f == nil {
		return w(gtx)
	}
	previous := f.scope
	f.scope = ID(gtx, path)
	dims := w(gtx)
	f.scope = previous
	return dims
}

func Scrollable(gtx layout.Context, list *layout.List, row int, w layout.Widget) layout.Dimensions {
	f := frameFor(gtx)
	if f == nil {
		return w(gtx)
	}
	previous := f.scroll
	f.scroll = func() { list.ScrollTo(row) }
	dims := w(gtx)
	f.scroll = previous
	return dims
}

func Parent(gtx layout.Context) platform.NodeID {
	if f := frameFor(gtx); f != nil {
		return f.parent
	}
	return ""
}

func Group(gtx layout.Context, n platform.SemanticNode, a Actions, w layout.Widget) layout.Dimensions {
	f := frameFor(gtx)
	if f == nil {
		return w(gtx)
	}
	i := f.register(n, a)
	scope, parent := f.scope, f.parent
	f.scope, f.parent = f.entries[i].node.ID, f.entries[i].node.ID
	dims := w(gtx)
	f.scope, f.parent = scope, parent
	mark(gtx, image.Rectangle{Max: dims.Size}, i)
	return dims
}

func Add(gtx layout.Context, bounds image.Rectangle, n platform.SemanticNode, a Actions) {
	if f := frameFor(gtx); f != nil {
		mark(gtx, bounds, f.register(n, a))
	}
}

func Offscreen(gtx layout.Context, n platform.SemanticNode, a Actions) {
	if f := frameFor(gtx); f != nil {
		i := f.register(n, a)
		f.entries[i].offscreen = true
	}
}

func Place(gtx layout.Context, bounds image.Rectangle, tag event.Tag) {
	if f := frameFor(gtx); f != nil {
		f.placementTags = append(f.placementTags, tag)
		mark(gtx, bounds, -len(f.placementTags))
	}
}

func Within(parent platform.NodeID, actions Actions) Actions {
	actions.parent = parent
	return actions
}

func ResumeAfterPublish(gtx layout.Context, local platform.NodeID, action platform.AccessibilityAction) {
	if f := frameFor(gtx); f != nil {
		f.resume = append(f.resume, platform.AccessibilityIntent{Node: ID(gtx, string(local)), Action: action})
	}
}

func (f *Frame) register(n platform.SemanticNode, a Actions) int {
	a.bind()
	if a.Editor != nil {
		a.editorRevision = a.Editor.Revision()
	}
	if a.Scroll == nil {
		a.Scroll = f.scroll
	}
	if n.ID == "" {
		n.ID = platform.NodeID(fmt.Sprintf("%p", a.Tag))
	}
	n.ID = f.scope + "/" + n.ID
	n.Parent = f.parent
	if a.parent != "" {
		n.Parent = f.scope + "/" + a.parent
	}
	if n.Role == platform.RoleSecureField {
		n.Value, n.Lines, n.Selection = "", nil, inputtext.Range{}
	} else if a.Editor != nil {
		draft := a.Editor.Draft()
		n.Value, n.Selection = draft.Text, draft.Selection
	}
	i := len(f.entries)
	f.entries = append(f.entries, registered{node: n, actions: a})
	return i
}

func (a *Actions) bind() {
	switch {
	case a.Editor != nil:
		a.Tag = a.Editor.Tag()
	case a.Click != nil:
		a.Tag = a.Click
	}
}

func mark(gtx layout.Context, bounds image.Rectangle, i int) {
	defer clip.Rect(bounds).Push(gtx.Ops).Pop()
	semantic.DescriptionOp(boundsMarker + strconv.Itoa(i)).Add(gtx.Ops)
}

func (f *Frame) Focus(gtx layout.Context) event.Tag {
	for i := range f.entries {
		a := f.entries[i].actions
		if a.Tag != nil && gtx.Focused(a.Tag) {
			return a.Tag
		}
	}
	return nil
}

func (f *Frame) Publish(gtx layout.Context, tree *platform.AccessibilityTree, focused event.Tag) error {
	f.place(gtx)
	s := tree.Snapshot()
	revision := uint64(1)
	if s != nil {
		revision = s.Revision() + 1
	}
	focus, actionsRevision := f.collect(s, focused)
	if s == nil || actionsRevision == revision || focus != s.Focus() || !reflect.DeepEqual(f.previous, f.next) {
		next, err := platform.NewSemanticSnapshot(revision, focus, f.next)
		if err != nil {
			return err
		}
		tree.Publish(next)
		f.previous, f.next = f.next, f.previous
	}
	f.active, f.actions = f.actions, f.active
	s = tree.Snapshot()
	for _, intent := range f.resume {
		intent.Revision = s.Revision()
		f.handle(gtx, s, intent)
	}
	return nil
}

func (f *Frame) place(gtx layout.Context) {
	f.geometry.Frame(gtx.Ops)
	f.markers = f.geometry.AppendSemantics(f.markers[:0])
	clear(f.placements)
	for _, marker := range f.markers {
		number, ok := strings.CutPrefix(marker.Desc.Description, boundsMarker)
		if !ok {
			continue
		}
		index, err := strconv.Atoi(number)
		if err == nil {
			f.placeMarker(index, marker.Desc.Bounds)
		}
	}
	for i := range f.entries {
		r := &f.entries[i]
		if bounds, ok := f.placements[r.actions.Tag]; ok {
			r.place(bounds, f.bounds)
		}
	}
}

func (f *Frame) placeMarker(index int, bounds image.Rectangle) {
	if index < 0 && index >= -len(f.placementTags) {
		if f.placements == nil {
			f.placements = make(map[event.Tag]image.Rectangle)
		}
		f.placements[f.placementTags[-index-1]] = bounds
	} else if index > 0 && index < len(f.entries) {
		f.entries[index].place(bounds, f.bounds)
	}
}

func (r *registered) place(bounds, viewport image.Rectangle) {
	if r.placed {
		return
	}
	r.placed = true
	r.node.Bounds = bounds.Intersect(viewport)
	for i := range r.node.Lines {
		line := &r.node.Lines[i]
		line.Bounds = line.Bounds.Add(bounds.Min)
		for j := range line.Clusters {
			line.Clusters[j].Left += bounds.Min.X
			line.Clusters[j].Right += bounds.Min.X
		}
	}
}

func (f *Frame) collect(previous *platform.SemanticSnapshot, focused event.Tag) (platform.NodeID, uint64) {
	clear(f.next)
	f.next = f.next[:0]
	clear(f.actions)
	var focus platform.NodeID
	var actionsRevision uint64
	for _, r := range f.entries {
		if !f.visible(r) {
			continue
		}
		r.clip(f.actions[r.node.Parent])
		r.actions.textSince = f.textRevision(previous, r.node, r.actions)
		actionsRevision = max(actionsRevision, r.actions.textSince)
		f.next = append(f.next, r.node)
		f.actions[r.node.ID] = r.actions
		if r.matchesFocus(focused) && r.actions.Address == nil {
			focus = r.node.ID
		}
	}
	if a, ok := f.actions[f.logicalFocus]; ok && focused != nil && a.Tag == focused {
		focus = f.logicalFocus
	} else {
		f.logicalFocus = ""
	}
	return focus, actionsRevision
}

func (r *registered) clip(parent Actions) {
	if parent.bounded {
		r.node.Bounds = r.node.Bounds.Intersect(parent.bounds)
	}
	r.actions.bounds, r.actions.bounded = r.node.Bounds, r.placed
}

func (r registered) matchesFocus(tag event.Tag) bool {
	return !r.node.Disabled && tag != nil && tag == r.actions.Tag
}

func (f *Frame) textRevision(previous *platform.SemanticSnapshot, n platform.SemanticNode, a Actions) uint64 {
	if a.Editor == nil && a.TextPosition == nil && a.Address == nil && a.ActivationKey == "" {
		return 0
	}
	if previous == nil {
		return 1
	}
	oldActions := f.active[n.ID]
	if !a.editorChanged(oldActions) && sameAccessibleText(previous, n, a) && a.ActivationKey == oldActions.ActivationKey {
		return max(1, oldActions.textSince)
	}
	return previous.Revision() + 1
}

func (a Actions) editorChanged(old Actions) bool {
	return a.Editor != nil && (old.Editor != a.Editor || old.editorRevision != a.editorRevision)
}

func sameAccessibleText(previous *platform.SemanticSnapshot, n platform.SemanticNode, a Actions) bool {
	old, exists := previous.Node(n.ID)
	return exists && old.Role == n.Role && old.TextRevision == n.TextRevision && old.TextGroup == n.TextGroup && (a.Address != nil || old.Value == n.Value)
}

func (f *Frame) visible(r registered) bool {
	if !r.placed && !r.offscreen {
		return false
	}
	if _, present := f.actions[r.node.ID]; present {
		return false
	}
	_, parent := f.actions[r.node.Parent]
	return r.node.Parent == "" || parent
}

func (f *Frame) dispatch(gtx layout.Context, tree *platform.AccessibilityTree) {
	s := tree.Snapshot()
	for range cap(tree.Intents()) {
		select {
		case i := <-tree.Intents():
			f.handle(gtx, s, i)
			tree.Handled(i)
		default:
			return
		}
	}
}

func (f *Frame) handle(gtx layout.Context, s *platform.SemanticSnapshot, i platform.AccessibilityIntent) {
	if s == nil {
		return
	}
	n, ok := s.Node(i.Node)
	a := f.active[i.Node]
	if !ok || !a.accepts(n, i) {
		return
	}
	if i.Action == platform.ActionSelectText && a.TextPosition != nil {
		f.selectText(s, n, a, i)
		return
	}
	if a.Address != nil && i.Action != platform.ActionActivate {
		f.address(gtx, a, i)
		return
	}
	if i.Action == platform.ActionFocus {
		f.logicalFocus = ""
	}
	a.perform(gtx, i)
}

func (a Actions) accepts(n platform.SemanticNode, i platform.AccessibilityIntent) bool {
	return !n.Disabled && !a.stale(i)
}

func (f *Frame) selectText(s *platform.SemanticSnapshot, n platform.SemanticNode, a Actions, i platform.AccessibilityIntent) {
	end, z := n, a
	if i.SelectionEnd != "" {
		end, _ = s.Node(i.SelectionEnd)
		z = f.active[i.SelectionEnd]
	}
	if !a.selectableThrough(z) || !z.accepts(end, i) {
		return
	}
	if !validCaret(n.Value, i.Selection.Start) || !validCaret(end.Value, i.Selection.End) {
		return
	}
	selection, err := a.positionsTo(z, i.Selection, n.ID == end.ID)
	if err != nil {
		return
	}
	a.Selection.Selection, a.Selection.Selected = selection, true
}

func (a Actions) positionsTo(end Actions, span inputtext.Range, sameNode bool) (document.Selection, error) {
	anchor, err := a.TextPosition(span.Start, document.After)
	if err != nil {
		return document.Selection{}, err
	}
	selection := document.Caret(anchor)
	if sameNode && span.Start == span.End {
		return selection, nil
	}
	focus, err := end.TextPosition(span.End, document.Before)
	return selection.Extend(focus), err
}

func (a Actions) selectableThrough(end Actions) bool {
	return a.Selection != nil && a.Selection == end.Selection && end.TextPosition != nil
}

func validCaret(text string, at int) bool {
	return inputtext.CheckRange(text, inputtext.Range{Start: at, End: at}) == nil
}

func (a Actions) stale(i platform.AccessibilityIntent) bool {
	if a.Address != nil {
		return i.Revision < a.textSince
	}
	if a.ActivationKey != "" && i.Action == platform.ActionActivate {
		return i.Revision < a.textSince
	}
	if i.Action != platform.ActionSelectText && i.Action != platform.ActionSetValue {
		return false
	}
	return i.Revision < a.textSince || a.Editor != nil && a.Editor.Revision() != a.editorRevision
}

func (f *Frame) address(gtx layout.Context, a Actions, i platform.AccessibilityIntent) {
	if a.Address(i.Action) && i.Action == platform.ActionFocus && a.Tag != nil {
		f.logicalFocus = i.Node
		gtx.Execute(key.FocusCmd{Tag: a.Tag})
	}
}

func (a Actions) perform(gtx layout.Context, i platform.AccessibilityIntent) {
	switch i.Action {
	case platform.ActionActivate:
		a.press(gtx)
	case platform.ActionFocus:
		if a.Tag != nil {
			gtx.Execute(key.FocusCmd{Tag: a.Tag})
		}
	case platform.ActionScrollIntoView:
		if a.Scroll != nil {
			a.Scroll()
		}
	case platform.ActionSetValue, platform.ActionSelectText:
		a.performEdit(i)
	case platform.ActionLoadText:
	}
}

func (a Actions) performEdit(i platform.AccessibilityIntent) {
	if err := a.edit(i); err != nil && a.Rejected != nil {
		a.Rejected(err)
	}
}

func (a Actions) press(gtx layout.Context) {
	switch {
	case a.Click != nil:
		a.Click.Click()
	case a.Activate != nil:
		a.Activate()
	case a.Editor != nil:
		gtx.Execute(key.FocusCmd{Tag: a.Editor.Tag()})
	}
}

func (a Actions) edit(i platform.AccessibilityIntent) error {
	if a.Editor == nil {
		return platform.ErrElementUnavailable
	}
	if i.Action == platform.ActionSelectText {
		return a.Editor.Select(i.Selection)
	}
	return a.Editor.Replace(inputtext.Range{End: len(a.Editor.Draft().Text)}, i.Value)
}
