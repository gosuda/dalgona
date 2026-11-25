//go:build linux

package platform

import (
	"slices"
	"unicode/utf8"

	"github.com/godbus/dbus/v5"
)

const (
	atspiEventObject   = "org.a11y.atspi.Event.Object"
	atspiEventWindow   = "org.a11y.atspi.Event.Window"
	atspiLivePolite    = 1
	atspiLiveAssertive = 2
)

type atspiEvent struct {
	path    dbus.ObjectPath
	iface   string
	member  string
	detail  string
	detail1 int32
	detail2 int32
	value   any
}

type atspiChange struct {
	bridge   *atspiBridge
	bus      *atspiBus
	previous *SemanticSnapshot
	next     *SemanticSnapshot
	before   *atspiIndex
	after    *atspiIndex
}

func (b *atspiBridge) SnapshotChanged(_, next *SemanticSnapshot) {
	b.mu.Lock()
	b.latest = next
	b.mu.Unlock()
	b.notify()
}

func (b *atspiBridge) Announced(n Announcement) {
	b.mu.Lock()
	if len(b.announcements) == atspiAnnouncementLimit {
		b.announcements = slices.Delete(b.announcements, 0, 1)
	}
	b.announcements = append(b.announcements, n)
	b.mu.Unlock()
	b.notify()
}

func (b *atspiBridge) notify() {
	select {
	case b.wake <- struct{}{}:
	default:
	}
}

func (b *atspiBridge) flush(bus *atspiBus, emitted *SemanticSnapshot) *SemanticSnapshot {
	b.mu.Lock()
	next, announcements := b.latest, b.announcements
	b.announcements = nil
	b.mu.Unlock()
	if next != emitted {
		change := atspiChange{bridge: b, bus: bus, previous: emitted, next: next, before: newATSPIIndex(emitted), after: b.indexOf(next)}
		change.emit()
	}
	for _, n := range announcements {
		bus.emit(atspiEvent{path: atspiAnnouncer(next), member: "Announcement", detail1: atspiPoliteness(n.Kind), value: n.Text})
	}
	return next
}

func (bus *atspiBus) emit(e atspiEvent) {
	value, iface := e.value, e.iface
	if value == nil {
		value = int32(0)
	}
	if iface == "" {
		iface = atspiEventObject
	}
	_ = bus.conn.Emit(e.path, iface+"."+e.member, e.detail, e.detail1, e.detail2, dbus.MakeVariant(value), map[string]dbus.Variant{})
}

func atspiPoliteness(kind AnnouncementKind) int32 {
	switch kind {
	case AnnounceProgress, AnnounceCompletion:
		return atspiLivePolite
	case AnnounceDecision, AnnounceFailure:
		return atspiLiveAssertive
	}
	return atspiLivePolite
}

func atspiAnnouncer(snap *SemanticSnapshot) dbus.ObjectPath {
	switch {
	case snap == nil:
		return atspiRootPath
	case snap.Focus() != "":
		return atspiNodePath(snap.Focus())
	}
	return atspiNodePath(snap.Root())
}

func atspiRootList(snap *SemanticSnapshot) []NodeID {
	if snap == nil {
		return nil
	}
	return []NodeID{snap.Root()}
}

func (c atspiChange) object(snap *SemanticSnapshot, kind atspiKind, node SemanticNode) *atspiObject {
	base := &atspiObject{bridge: c.bridge, bus: c.bus, snap: snap}
	return base.object(kind, node)
}

func (c atspiChange) emit() {
	c.cacheAdded()
	c.window()
	if c.previous != nil {
		atspiWalk(c.next, c.node)
	}
	c.focus()
	c.cacheRemoved()
}

func (c atspiChange) cacheAdded() {
	atspiWalk(c.next, func(n SemanticNode) {
		if !atspiHas(c.previous, n.ID) {
			c.bus.emitCache("AddAccessible", c.object(c.next, atspiNodeKind, n).cacheItem())
		}
	})
}

func (c atspiChange) cacheRemoved() {
	atspiWalk(c.previous, func(n SemanticNode) {
		if !atspiHas(c.next, n.ID) {
			c.bus.emitCache("RemoveAccessible", c.bus.ref(atspiNodePath(n.ID)))
		}
	})
}

func (c atspiChange) window() {
	c.children(atspiFramePath, atspiRootList(c.previous), atspiRootList(c.next))
	before, after := c.object(c.previous, atspiFrameKind, SemanticNode{}), c.object(c.next, atspiFrameKind, SemanticNode{})
	if before.name() != after.name() {
		c.bus.emit(atspiEvent{path: atspiFramePath, member: "PropertyChange", detail: "accessible-name", value: after.name()})
	}
	c.stateChanges(atspiFramePath, before.states(), after.states())
}

func (c atspiChange) node(n SemanticNode) {
	old, ok := c.previous.Node(n.ID)
	if !ok {
		return
	}
	path := atspiNodePath(n.ID)
	c.children(path, c.previous.Children(n.ID), c.next.Children(n.ID))
	c.properties(path, old, n)
	c.stateChanges(path, atspiNodeStates(old, c.before.context(old)), atspiNodeStates(n, c.after.context(n)))
	c.selection(path, old, n)
	c.text(path, old, n)
	c.caret(path, old, n)
	if n.TextContainer {
		c.textGroup(path, n.ID)
	}
}

func (c atspiChange) textGroup(path dbus.ObjectPath, id NodeID) {
	content, selection := c.next.textGroupChanges(c.previous, id)
	if content {
		c.bus.emit(atspiEvent{path: path, member: "VisibleDataChanged", value: ""})
	}
	if selection {
		c.bus.emit(atspiEvent{path: path, member: "TextSelectionChanged", value: ""})
	}
}

func (c atspiChange) selection(path dbus.ObjectPath, old, n SemanticNode) {
	if !slices.Equal(atspiSelectedChildren(c.previous, old), atspiSelectedChildren(c.next, n)) {
		c.bus.emit(atspiEvent{path: path, member: "SelectionChanged", value: ""})
	}
}

func (c atspiChange) children(path dbus.ObjectPath, before, after []NodeID) {
	if slices.Equal(before, after) {
		return
	}
	removed, added := atspiChildEdits(before, after)
	for _, i := range slices.Backward(removed) {
		c.bus.emit(atspiEvent{path: path, member: "ChildrenChanged", detail: "remove", detail1: clampInt32(i), value: c.bus.ref(atspiNodePath(before[i]))})
	}
	for _, i := range added {
		c.bus.emit(atspiEvent{path: path, member: "ChildrenChanged", detail: "add", detail1: clampInt32(i), value: c.bus.ref(atspiNodePath(after[i]))})
	}
}

func atspiChildEdits(before, after []NodeID) (removed, added []int) {
	inBefore, inAfter := atspiMembers(before), atspiMembers(after)
	reordered := !slices.Equal(atspiKept(before, inAfter), atspiKept(after, inBefore))
	return atspiMissing(before, inAfter, reordered), atspiMissing(after, inBefore, reordered)
}

func atspiMembers(ids []NodeID) map[NodeID]bool {
	members := make(map[NodeID]bool, len(ids))
	for _, id := range ids {
		members[id] = true
	}
	return members
}

func atspiKept(ids []NodeID, other map[NodeID]bool) []NodeID {
	kept := make([]NodeID, 0, len(ids))
	for _, id := range ids {
		if other[id] {
			kept = append(kept, id)
		}
	}
	return kept
}

func atspiMissing(ids []NodeID, other map[NodeID]bool, all bool) []int {
	var missing []int
	for i, id := range ids {
		if all || !other[id] {
			missing = append(missing, i)
		}
	}
	return missing
}

func (c atspiChange) properties(path dbus.ObjectPath, old, n SemanticNode) {
	if old.Name != n.Name {
		c.bus.emit(atspiEvent{path: path, member: "PropertyChange", detail: "accessible-name", value: n.Name})
	}
	if old.description() != n.description() {
		c.bus.emit(atspiEvent{path: path, member: "PropertyChange", detail: "accessible-description", value: n.description()})
	}
	if old.Role != n.Role {
		c.bus.emit(atspiEvent{path: path, member: "PropertyChange", detail: "accessible-role", value: atspiRoleOf(n.Role).value})
	}
	if old.Parent != n.Parent {
		parent := c.bus.ref(atspiNodePath(n.Parent))
		if n.Parent == "" {
			parent = c.bus.ref(atspiFramePath)
		}
		c.bus.emit(atspiEvent{path: path, member: "PropertyChange", detail: "accessible-parent", value: parent})
	}
}

func (c atspiChange) stateChanges(path dbus.ObjectPath, before, after atspiStates) {
	for _, s := range atspiStateEvents {
		if on := after.has(s.state); on != before.has(s.state) {
			c.bus.emit(atspiEvent{path: path, member: "StateChanged", detail: s.name, detail1: atspiFlag(on)})
		}
	}
}

func atspiFlag(on bool) int32 {
	if on {
		return 1
	}
	return 0
}

func (c atspiChange) focus() {
	before, after := atspiFocusOf(c.previous), atspiFocusOf(c.next)
	if before == after {
		return
	}
	if before == "" {
		c.activation("Activate", 1)
	}
	if atspiHas(c.next, before) {
		c.bus.emit(atspiEvent{path: atspiNodePath(before), member: "StateChanged", detail: "focused"})
	}
	if after == "" {
		c.activation("Deactivate", 0)
		return
	}
	c.bus.emit(atspiEvent{path: atspiNodePath(after), member: "StateChanged", detail: "focused", detail1: 1})
}

func (c atspiChange) activation(member string, active int32) {
	frame := c.object(c.next, atspiFrameKind, SemanticNode{})
	c.bus.emit(atspiEvent{path: atspiFramePath, iface: atspiEventWindow, member: member, value: frame.name()})
	c.bus.emit(atspiEvent{path: atspiFramePath, member: "StateChanged", detail: "active", detail1: active})
}

func (c atspiChange) text(path dbus.ObjectPath, old, n SemanticNode) {
	before, after := atspiTextOf(old), atspiTextOf(n)
	if before == after {
		return
	}
	start, removed, inserted := atspiTextEdit(before, after)
	offset := clampInt32(utf8.RuneCountInString(before[:start]))
	if removed != "" {
		c.bus.emit(atspiEvent{path: path, member: "TextChanged", detail: "delete", detail1: offset, detail2: clampInt32(utf8.RuneCountInString(removed)), value: removed})
	}
	if inserted != "" {
		c.bus.emit(atspiEvent{path: path, member: "TextChanged", detail: "insert", detail1: offset, detail2: clampInt32(utf8.RuneCountInString(inserted)), value: inserted})
	}
}

func atspiTextEdit(before, after string) (start int, removed, inserted string) {
	start = atspiCommonPrefix(before, after)
	end := atspiCommonSuffix(before[start:], after[start:])
	return start, before[start : len(before)-end], after[start : len(after)-end]
}

func atspiCommonPrefix(a, b string) int {
	n := 0
	for n < len(a) && n < len(b) && a[n] == b[n] {
		n++
	}
	for n > 0 && (n < len(a) && !utf8.RuneStart(a[n]) || n < len(b) && !utf8.RuneStart(b[n])) {
		n--
	}
	return n
}

func atspiCommonSuffix(a, b string) int {
	n := 0
	for n < len(a) && n < len(b) && a[len(a)-1-n] == b[len(b)-1-n] {
		n++
	}
	for n > 0 && !utf8.RuneStart(a[len(a)-n]) {
		n--
	}
	return n
}

func (c atspiChange) caret(path dbus.ObjectPath, old, n SemanticNode) {
	if !atspiHasText(n) {
		return
	}
	beforeStart, beforeEnd, errBefore := atspiCharacterRange(old)
	afterStart, afterEnd, errAfter := atspiCharacterRange(n)
	if errBefore != nil || errAfter != nil {
		return
	}
	if beforeEnd != afterEnd {
		c.bus.emit(atspiEvent{path: path, member: "TextCaretMoved", detail1: afterEnd})
	}
	if (beforeStart != afterStart || beforeEnd != afterEnd) && (beforeStart != beforeEnd || afterStart != afterEnd) {
		c.bus.emit(atspiEvent{path: path, member: "TextSelectionChanged", value: ""})
	}
}
