//go:build windows && (amd64 || arm64)

package platform

import (
	"sync"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	uiaIsReadOnlyAttribute int32 = 40015
	uiaSingleTextSelection int32 = 1
)

type uiaTextRange struct {
	iface   uiaInterface
	refs    uiaReferences
	element *uiaElement
	mu      sync.Mutex
	span    inputtext.Range
	logical TextRange
}

func newUIATextRange(el *uiaElement, span inputtext.Range) *uiaTextRange {
	r := &uiaTextRange{element: el, span: span}
	r.iface = uiaInterface{&uiaCallbacks.textRange[0], r}
	return r
}

func (r *uiaTextRange) identity() *uiaInterface { return &r.iface }

func (r *uiaTextRange) references() *uiaReferences { return &r.refs }

func (r *uiaTextRange) query(iid *windows.GUID) *uiaInterface {
	if *iid == uiaIIDUnknown || *iid == uiaIIDTextRange {
		return &r.iface
	}
	return nil
}

func (r *uiaTextRange) current() inputtext.Range {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.span
}

func (r *uiaTextRange) store(span inputtext.Range) {
	r.mu.Lock()
	r.span = span
	r.mu.Unlock()
}

type uiaRangeQuery struct {
	uiaQuery
	rng     *uiaTextRange
	text    string
	span    inputtext.Range
	logical TextRange
}

func uiaRangeEnter(this *uiaInterface) (uiaRangeQuery, uintptr) {
	r, _ := this.owner.(*uiaTextRange)
	if r == nil {
		return uiaRangeQuery{}, uiaUnavailable
	}
	q, hr := uiaEnter(&r.element.simple)
	if hr != uiaOK {
		return uiaRangeQuery{}, hr
	}
	return q.rangeQuery(r)
}

func uiaEnterText(this *uiaInterface) (uiaQuery, uintptr) {
	q, hr := uiaEnter(this)
	if hr != uiaOK {
		return q, hr
	}
	switch q.node.TextState {
	case TextUncached:
		if hr := q.request(AccessibilityIntent{Action: ActionLoadText}); hr != uiaOK {
			return q, hr
		}
		return q, uiaRetryLater
	case TextLoading:
		return q, uiaRetryLater
	case TextFailed:
		return q, uiaUnavailable
	case TextReady:
		return q, uiaOK
	}
	return q, uiaUnavailable
}

func (rq uiaRangeQuery) peer(p uintptr) (inputtext.Range, bool) {
	other, _ := uiaLookup(p).(*uiaTextRange)
	if other == nil || other.element != rq.rng.element {
		return inputtext.Range{}, false
	}
	return uiaSnapRange(rq.text, other.current()), true
}

func (rq uiaRangeQuery) readOnly() bool {
	return rq.node.Role != RoleTextField || rq.node.Disabled
}

func (q uiaQuery) handOutRange(out *uintptr, span inputtext.Range) uintptr {
	r := newUIATextRange(q.element, span)
	return uiaHandOut(out, r, &r.iface)
}

func (q uiaQuery) rangeArray(out *uintptr, span inputtext.Range) uintptr {
	r := newUIATextRange(q.element, span)
	return uiaObjectArray([]*uiaInterface{&r.iface}, out)
}

func uiaTextSelection(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnterText(this)
	if hr != uiaOK {
		return hr
	}
	if q.node.TextContainer {
		return q.logicalArray(out, q.textView().selected())
	}
	return q.rangeArray(out, uiaSnapRange(q.node.Value, q.node.Selection))
}

func uiaTextVisibleRanges(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnterText(this)
	if hr != uiaOK {
		return hr
	}
	if q.node.TextContainer {
		return q.visibleRanges(out)
	}
	return q.rangeArray(out, inputtext.Range{End: len(q.node.Value)})
}

func uiaRangeFromChild(this *uiaInterface, child uintptr, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnterText(this)
	if hr != uiaOK {
		return hr
	}
	el, _ := uiaLookup(child).(*uiaElement)
	if !q.node.TextContainer || el == nil || el.bridge != q.element.bridge {
		return uiaInvalidArg
	}
	if _, err := q.textView().part(el.id); err != nil {
		return uiaInvalidArg
	}
	return q.handOutLogical(out, partRange(el.id))
}

func uiaRangeFromPoint(this *uiaInterface, x, y uintptr, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnterText(this)
	if hr != uiaOK {
		return hr
	}
	p, ok := q.element.bridge.clientPoint(x, y)
	if !ok {
		return uiaUnavailable
	}
	if q.node.TextContainer {
		point := q.groupPoint(p)
		return q.handOutLogical(out, TextRange{Start: point, End: point})
	}
	at := uiaPointOffset(q.node, p)
	return q.handOutRange(out, inputtext.Range{Start: at, End: at})
}

func uiaDocumentRange(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	q, hr := uiaEnterText(this)
	if hr != uiaOK {
		return hr
	}
	if q.node.TextContainer {
		return q.handOutLogical(out, q.textView().whole())
	}
	if q.node.TextGroup != "" {
		return q.handOutLogical(out, partRange(q.node.ID))
	}
	return q.handOutRange(out, inputtext.Range{End: len(q.node.Value)})
}

func uiaSupportedTextSelection(this *uiaInterface, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = uiaSingleTextSelection
	q, hr := uiaEnter(this)
	if q.node.TextContainer && !q.node.TextSelectable {
		*out = 0
	}
	return hr
}

func uiaRangeClone(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return rq.handOutLogical(out, rq.logical)
	}
	return rq.handOutRange(out, rq.span)
}

func uiaRangeCompare(this *uiaInterface, other uintptr, out *int32) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		equal, hr := rq.compareRange(other)
		*out = uiaWinBool(equal)
		return hr
	}
	span, ok := rq.peer(other)
	if !ok {
		return uiaInvalidArg
	}
	*out = uiaWinBool(span == rq.span)
	return uiaOK
}

func uiaRangeCompareEndpoints(this *uiaInterface, endpoint int32, target uintptr, targetEndpoint int32, out *int32) uintptr {
	if out == nil || !uiaValidEndpoint(endpoint) || !uiaValidEndpoint(targetEndpoint) {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		order, hr := rq.comparePoint(endpoint, target, targetEndpoint)
		*out = clampInt32(order)
		return hr
	}
	span, ok := rq.peer(target)
	if !ok {
		return uiaInvalidArg
	}
	mine, theirs := uiaEndpointOf(rq.span, endpoint), uiaEndpointOf(span, targetEndpoint)
	switch {
	case mine < theirs:
		*out = -1
	case mine > theirs:
		*out = 1
	}
	return uiaOK
}

func uiaRangeExpand(this *uiaInterface, unit int32) uintptr {
	u, ok := uiaTextUnits[unit]
	if !ok {
		return uiaInvalidArg
	}
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		span, err := rq.textView().uiaExpand(rq.logical.Start, u)
		if err == nil {
			rq.rng.storeLogical(span)
		}
		return uiaRangeStatus(err)
	}
	rq.rng.store(uiaExpand(rq.text, rq.span, u))
	return uiaOK
}

func uiaRangeFindAttribute(this *uiaInterface, attribute int32, value *uiaVariant, _ int32, out *uintptr) uintptr {
	if out == nil || value == nil {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if attribute != uiaIsReadOnlyAttribute || value.kind != uiaVTBool || value.flag() != rq.readOnly() {
		return uiaOK
	}
	if rq.logical.Start.Node != "" {
		return rq.handOutLogical(out, rq.logical)
	}
	return rq.handOutRange(out, rq.span)
}

func uiaRangeFindText(this *uiaInterface, text *uint16, backward, ignoreCase int32, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	needle, ok := uiaFromBSTR(text)
	if !ok {
		return uiaInvalidArg
	}
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return rq.findLogical(needle, backward != 0, ignoreCase != 0, out)
	}
	found, ok := uiaFind(rq.text, rq.span, needle, backward != 0, ignoreCase != 0)
	if !ok {
		return uiaOK
	}
	return rq.handOutRange(out, found)
}

func uiaRangeAttribute(this *uiaInterface, attribute int32, out *uiaVariant) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = uiaVariant{}
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if attribute == uiaIsReadOnlyAttribute {
		out.setBool(rq.readOnly())
		return uiaOK
	}
	hr, _, _ = procUiaGetReservedNotSupportedValue.Call(uintptr(unsafe.Pointer(&out.value))) //nolint:gosec // UiaGetReservedNotSupportedValue writes the reserved IUnknown into the variant
	if !uiaFailed(hr) {
		out.kind = uiaVTUnknown
	}
	return hr
}

func uiaRangeBounds(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	boxes := rq.boxes()
	values := make([]float64, 0, 4*len(boxes))
	for _, box := range boxes {
		rect, ok := rq.element.bridge.screenRect(box)
		if !ok {
			return uiaUnavailable
		}
		values = append(values, rect.left, rect.top, rect.width, rect.height)
	}
	return uiaDoubleArray(values, out)
}

func uiaRangeEnclosingElement(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	return uiaHandOut(out, rq.element, &rq.element.simple)
}

func uiaRangeGetText(this *uiaInterface, maxLength int32, out *uintptr) uintptr {
	if out == nil || maxLength < -1 {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	text, hr := rq.rangeText(int(maxLength))
	if hr != uiaOK {
		return hr
	}
	if *out = uiaBSTR(text); *out == 0 {
		return uiaOutOfMemory
	}
	return uiaOK
}

func uiaRangeMove(this *uiaInterface, unit, count int32, out *int32) uintptr {
	u, ok := uiaTextUnits[unit]
	if out == nil || !ok {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return rq.move(u, count, out)
	}
	span, moved := uiaMove(rq.text, rq.span, u, count)
	rq.rng.store(span)
	*out = moved
	return uiaOK
}

func uiaRangeMoveEndpointByUnit(this *uiaInterface, endpoint, unit, count int32, out *int32) uintptr {
	u, ok := uiaTextUnits[unit]
	if out == nil || !ok || !uiaValidEndpoint(endpoint) {
		return uiaInvalidArg
	}
	*out = 0
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return rq.moveEndpoint(endpoint, u, count, out)
	}
	span, moved := uiaMoveEndpoint(rq.text, rq.span, endpoint, u, count)
	rq.rng.store(span)
	*out = moved
	return uiaOK
}

func uiaRangeMoveEndpointByRange(this *uiaInterface, endpoint int32, target uintptr, targetEndpoint int32) uintptr {
	if !uiaValidEndpoint(endpoint) || !uiaValidEndpoint(targetEndpoint) {
		return uiaInvalidArg
	}
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return rq.moveToRange(endpoint, target, targetEndpoint)
	}
	span, ok := rq.peer(target)
	if !ok {
		return uiaInvalidArg
	}
	rq.rng.store(uiaWithEndpoint(rq.span, endpoint, uiaEndpointOf(span, targetEndpoint)))
	return uiaOK
}

func uiaRangeSelect(this *uiaInterface) uintptr {
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		return uiaRangeStatus(rq.tree.SelectTextRange(rq.snapshot, rq.logical))
	}
	return rq.request(AccessibilityIntent{Action: ActionSelectText, Selection: rq.span})
}

func uiaRangeAddToSelection(this *uiaInterface) uintptr {
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		order, err := rq.textView().compare(rq.logical.Start, rq.logical.End)
		if err != nil {
			return uiaRangeStatus(err)
		}
		if order != 0 {
			return uiaInvalidOperation
		}
		return uiaRangeStatus(rq.tree.SelectTextRange(rq.snapshot, rq.logical))
	}
	if rq.span.Start != rq.span.End {
		return uiaInvalidOperation
	}
	return rq.request(AccessibilityIntent{Action: ActionSelectText, Selection: rq.span})
}

func uiaRangeRemoveFromSelection(this *uiaInterface) uintptr {
	return uiaInvalidOperation
}

func uiaRangeScrollIntoView(this *uiaInterface, _ int32) uintptr {
	rq, hr := uiaRangeEnter(this)
	if hr != uiaOK {
		return hr
	}
	if rq.logical.Start.Node != "" {
		tree := rq.element.bridge.tree.Load()
		if tree == nil {
			return uiaUnavailable
		}
		i := AccessibilityIntent{Node: rq.logical.Start.Node, Revision: rq.snapshot.Revision(), Action: ActionScrollIntoView}
		return uiaRangeStatus(tree.Request(i))
	}
	return rq.request(AccessibilityIntent{Action: ActionScrollIntoView, Selection: rq.span})
}

func uiaRangeChildren(this *uiaInterface, out *uintptr) uintptr {
	if out == nil {
		return uiaInvalidArg
	}
	*out = 0
	if _, hr := uiaRangeEnter(this); hr != uiaOK {
		return hr
	}
	return uiaObjectArray(nil, out)
}
