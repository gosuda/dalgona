//go:build windows && (amd64 || arm64)

package platform

import (
	"errors"
	"image"
	"math"
	"sync/atomic"
	"unsafe"

	"golang.org/x/sys/windows"
)

const uiaServerSideProvider int32 = 2

type uiaElement struct {
	simple    uiaInterface
	fragment  uiaInterface
	root      uiaInterface
	invoke    uiaInterface
	value     uiaInterface
	toggle    uiaInterface
	selection uiaInterface
	expand    uiaInterface
	scroll    uiaInterface
	text      uiaInterface
	grid      uiaInterface
	gridItem  uiaInterface
	table     uiaInterface
	tableItem uiaInterface
	refs      uiaReferences
	bridge    *uiaBridge
	id        NodeID
	role      Role
	window    bool
	gone      atomic.Bool
}

type uiaPatternSlot struct {
	iid     *windows.GUID
	pattern int32
	iface   func(el *uiaElement) *uiaInterface
}

var uiaPatternSlots = []uiaPatternSlot{
	{&uiaIIDInvoke, uiaInvokePattern, func(el *uiaElement) *uiaInterface { return &el.invoke }},
	{&uiaIIDValue, uiaValuePattern, func(el *uiaElement) *uiaInterface { return &el.value }},
	{&uiaIIDToggle, uiaTogglePattern, func(el *uiaElement) *uiaInterface { return &el.toggle }},
	{&uiaIIDSelectionItem, uiaSelectionItemPattern, func(el *uiaElement) *uiaInterface { return &el.selection }},
	{&uiaIIDExpandCollapse, uiaExpandCollapsePattern, func(el *uiaElement) *uiaInterface { return &el.expand }},
	{&uiaIIDScrollItem, uiaScrollItemPattern, func(el *uiaElement) *uiaInterface { return &el.scroll }},
	{&uiaIIDText, uiaTextPattern, func(el *uiaElement) *uiaInterface { return &el.text }},
	{&uiaIIDGrid, uiaGridPattern, func(el *uiaElement) *uiaInterface { return &el.grid }},
	{&uiaIIDGridItem, uiaGridItemPattern, func(el *uiaElement) *uiaInterface { return &el.gridItem }},
	{&uiaIIDTable, uiaTablePattern, func(el *uiaElement) *uiaInterface { return &el.table }},
	{&uiaIIDTableItem, uiaTableItemPattern, func(el *uiaElement) *uiaInterface { return &el.tableItem }},
}

func newUIAElement(b *uiaBridge, id NodeID, role Role) *uiaElement {
	el := &uiaElement{bridge: b, id: id, role: role, window: id == ""}
	v := &uiaCallbacks
	el.simple = uiaInterface{&v.simple[0], el}
	el.fragment = uiaInterface{&v.fragment[0], el}
	el.root = uiaInterface{&v.root[0], el}
	el.invoke = uiaInterface{&v.invoke[0], el}
	el.value = uiaInterface{&v.value[0], el}
	el.toggle = uiaInterface{&v.toggle[0], el}
	el.selection = uiaInterface{&v.selection[0], el}
	el.expand = uiaInterface{&v.expand[0], el}
	el.scroll = uiaInterface{&v.scroll[0], el}
	el.text = uiaInterface{&v.text[0], el}
	el.grid = uiaInterface{&v.grid[0], el}
	el.gridItem = uiaInterface{&v.gridItem[0], el}
	el.table = uiaInterface{&v.table[0], el}
	el.tableItem = uiaInterface{&v.tableItem[0], el}
	return el
}

func (el *uiaElement) identity() *uiaInterface { return &el.simple }

func (el *uiaElement) references() *uiaReferences { return &el.refs }

func (el *uiaElement) query(iid *windows.GUID) *uiaInterface {
	switch *iid {
	case uiaIIDUnknown, uiaIIDSimple:
		return &el.simple
	case uiaIIDFragment:
		return &el.fragment
	case uiaIIDFragmentRoot:
		if el.window {
			return &el.root
		}
		return nil
	}
	for _, slot := range uiaPatternSlots {
		if *slot.iid == *iid && el.supports(slot.pattern) {
			return slot.iface(el)
		}
	}
	return nil
}

func (el *uiaElement) supports(pattern int32) bool {
	tree := el.bridge.tree.Load()
	if el.window || tree == nil {
		return false
	}
	n, ok := uiaNodeIn(tree.Snapshot(), el.id)
	return ok && n.Role == el.role && uiaSupportsPattern(n, pattern)
}

func (el *uiaElement) patternIface(pattern int32) *uiaInterface {
	for _, slot := range uiaPatternSlots {
		if slot.pattern == pattern && el.supports(pattern) {
			return slot.iface(el)
		}
	}
	return nil
}

type uiaQuery struct {
	element  *uiaElement
	snapshot *SemanticSnapshot
	node     SemanticNode
	tree     *AccessibilityTree
}

func uiaEnter(this *uiaInterface) (uiaQuery, uintptr) {
	el, _ := this.owner.(*uiaElement)
	if el == nil || el.gone.Load() {
		return uiaQuery{}, uiaUnavailable
	}
	tree := el.bridge.tree.Load()
	if tree == nil {
		return uiaQuery{}, uiaUnavailable
	}
	q := uiaQuery{element: el, snapshot: tree.Snapshot(), tree: tree}
	if el.window {
		return q, uiaOK
	}
	n, ok := uiaNodeIn(q.snapshot, el.id)
	if !ok || n.Role != el.role {
		return uiaQuery{}, uiaUnavailable
	}
	q.node = n
	return q, uiaOK
}

func (q uiaQuery) request(intent AccessibilityIntent) uintptr {
	if q.node.Disabled && intent.Action != ActionScrollIntoView {
		return uiaNotEnabled
	}
	tree := q.element.bridge.tree.Load()
	if tree == nil {
		return uiaUnavailable
	}
	intent.Node, intent.Revision = q.element.id, q.snapshot.Revision()
	err := tree.Request(intent)
	switch {
	case err == nil:
		return uiaOK
	case errors.Is(err, ErrAccessibilityBusy):
		return uiaRetryLater
	case errors.Is(err, ErrElementUnavailable):
		return uiaUnavailable
	}
	return uiaFail
}

func (q uiaQuery) handOut(out *uintptr, id NodeID) uintptr {
	b := q.element.bridge
	if id == "" {
		return uiaHandOut(out, b.window, &b.window.fragment)
	}
	n, _ := q.snapshot.Node(id)
	el := b.element(id, n.Role)
	return uiaHandOut(out, el, &el.fragment)
}

func (q uiaQuery) handOutSimple(out *uintptr, id NodeID) uintptr {
	n, _ := q.snapshot.Node(id)
	el := q.element.bridge.element(id, n.Role)
	return uiaHandOut(out, el, &el.simple)
}

func uiaAct(this *uiaInterface, intent AccessibilityIntent) uintptr {
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	return q.request(intent)
}

func uiaWinBool(b bool) int32 {
	if b {
		return 1
	}
	return 0
}

func uiaProviderOptions(_ *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = uiaServerSideProvider
	return uiaOK
}

func uiaGetPatternProvider(this *uiaInterface, pattern int32, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	iface := q.element.patternIface(pattern)
	if iface == nil {
		return uiaOK
	}
	return uiaHandOut(out, q.element, iface)
}

func uiaGetPropertyValue(this *uiaInterface, property int32, out *uiaVariant) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = uiaVariant{}
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.element.window {
		return hr
	}
	return q.variant(out, uiaProperty(q.snapshot, q.node, property))
}

func (q uiaQuery) variant(out *uiaVariant, v uiaValue) uintptr {
	if v.kind == uiaElements {
		return q.elementsVariant(out, v.nodes)
	}
	return out.set(v)
}

func (q uiaQuery) elementsVariant(out *uiaVariant, ids []NodeID) uintptr {
	var array uintptr
	if hr := q.simpleArray(&array, ids); uiaFailed(hr) {
		return hr
	}
	out.kind, out.value = uiaVTArray|uiaVTUnknown, array
	return uiaOK
}

func uiaHostProvider(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK || !q.element.window {
		return hr
	}
	hr, _, _ = procUiaHostProviderFromHwnd.Call(q.element.bridge.hwnd, uintptr(unsafe.Pointer(out))) //nolint:gosec // UiaHostProviderFromHwnd writes the host provider pointer
	return hr
}

func uiaNavigate(this *uiaInterface, direction int32, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	target, window := uiaNavigation(q.snapshot, q.element.id, direction)
	if target == "" && !window {
		return uiaOK
	}
	return q.handOut(out, target)
}

func uiaGetRuntimeID(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.element.window {
		return hr
	}
	id := uiaRuntimeID(q.element.id)
	return uiaIntArray(id[:], out)
}

func uiaBoundingRectangle(this *uiaInterface, out *uiaRect) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = uiaRect{}
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.element.window || q.node.Bounds.Empty() {
		return hr
	}
	rect, ok := q.element.bridge.screenRect(q.node.Bounds)
	if !ok {
		return uiaUnavailable
	}
	*out = rect
	return uiaOK
}

func uiaEmbeddedRoots(_ *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	return uiaOK
}

func uiaSetFocus(this *uiaInterface) uintptr {
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.element.window {
		return hr
	}
	return q.request(AccessibilityIntent{Action: ActionFocus})
}

func uiaFragmentRoot(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	b := q.element.bridge
	return uiaHandOut(out, b.window, &b.window.root)
}

func uiaElementFromPoint(this *uiaInterface, x, y uintptr, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return hr
	}
	p, ok := q.element.bridge.clientPoint(x, y)
	if !ok {
		return uiaUnavailable
	}
	if id := uiaHitTest(q.snapshot, p); id != "" {
		return q.handOut(out, id)
	}
	return uiaOK
}

func uiaGetFocus(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.snapshot == nil || q.snapshot.Focus() == "" {
		return hr
	}
	return q.handOut(out, q.snapshot.Focus())
}

func uiaInvoke(this *uiaInterface) uintptr {
	return uiaAct(this, AccessibilityIntent{Action: ActionActivate})
}

func uiaSetValue(this *uiaInterface, text *uint16) uintptr {
	value, ok := uiaFromWide(text)
	if !ok {
		return uiaInvalidArg
	}
	return uiaAct(this, AccessibilityIntent{Action: ActionSetValue, Value: value})
}

func uiaGetValue(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnter(this)
	switch {
	case hr != uiaOK:
		return hr
	case q.node.Role == RoleSecureField:
		return uiaAccessDenied
	}
	if *out = uiaBSTR(q.node.Value); *out == 0 {
		return uiaOutOfMemory
	}
	return uiaOK
}

func uiaIsReadOnly(this *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	q, hr := uiaEnter(this)
	*out = uiaWinBool(q.node.Disabled)
	return hr
}

func uiaToggle(this *uiaInterface) uintptr {
	return uiaAct(this, AccessibilityIntent{Action: ActionActivate})
}

func uiaGetToggleState(this *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	q, hr := uiaEnter(this)
	*out = uiaToggleState(q.node)
	return hr
}

func uiaSelect(this *uiaInterface) uintptr {
	q, hr := uiaEnter(this)
	if hr != uiaOK || q.node.selected() {
		return hr
	}
	return q.request(AccessibilityIntent{Action: ActionActivate})
}

func uiaAddToSelection(this *uiaInterface) uintptr {
	return uiaSelect(this)
}

func uiaRemoveFromSelection(this *uiaInterface) uintptr {
	q, hr := uiaEnter(this)
	if hr == uiaOK && q.node.selected() {
		return uiaInvalidOperation
	}
	return hr
}

func uiaIsSelected(this *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	q, hr := uiaEnter(this)
	*out = uiaWinBool(q.node.selected())
	return hr
}

func uiaExpandNode(this *uiaInterface) uintptr { return uiaSetExpanded(this, true) }

func uiaCollapseNode(this *uiaInterface) uintptr { return uiaSetExpanded(this, false) }

func uiaSetExpanded(this *uiaInterface, expanded bool) uintptr {
	q, hr := uiaEnter(this)
	switch {
	case hr != uiaOK:
		return hr
	case !q.node.Expandable:
		return uiaInvalidOperation
	case q.node.Expanded == expanded:
		return uiaOK
	}
	return q.request(AccessibilityIntent{Action: ActionActivate})
}

func uiaGetExpandState(this *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	q, hr := uiaEnter(this)
	*out = uiaExpandState(q.node)
	return hr
}

func uiaSelectionContainer(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	_, hr := uiaEnter(this)
	return hr
}

func uiaScrollIntoView(this *uiaInterface) uintptr {
	return uiaAct(this, AccessibilityIntent{Action: ActionScrollIntoView})
}

func (b *uiaBridge) screenRect(r image.Rectangle) (uiaRect, bool) {
	origin, ok := uiaClientOrigin(b.hwnd)
	if !ok {
		return uiaRect{}, false
	}
	r = r.Add(image.Pt(int(origin.x), int(origin.y)))
	return uiaRect{left: float64(r.Min.X), top: float64(r.Min.Y), width: float64(r.Dx()), height: float64(r.Dy())}, true
}

func (b *uiaBridge) clientPoint(x, y uintptr) (image.Point, bool) {
	origin, ok := uiaClientOrigin(b.hwnd)
	if !ok {
		return image.Point{}, false
	}
	sx, sy := math.Float64frombits(uint64(x)), math.Float64frombits(uint64(y))
	return image.Pt(int(math.Floor(sx))-int(origin.x), int(math.Floor(sy))-int(origin.y)), true
}
