//go:build windows && (amd64 || arm64)

package platform

import (
	"errors"
	"image"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func (q uiaQuery) textView() textView {
	root := q.node.TextGroup
	if q.node.TextContainer {
		root = q.node.ID
	}
	return q.tree.textView(q.snapshot, root)
}

func (q uiaQuery) rangeQuery(r *uiaTextRange) (uiaRangeQuery, uintptr) {
	span, logical := r.ranges()
	view := q.textView()
	if logical.Start.Node != "" || q.node.TextContainer {
		return uiaRangeQuery{uiaQuery: q, rng: r, logical: logical}, uiaOK
	}
	if q.node.TextState != TextReady {
		_, err := view.tree.QueryText(q.node.ID, q.snapshot.Revision())
		return uiaRangeQuery{}, uiaRangeStatus(err)
	}
	text := q.node.Value
	span = uiaSnapRange(text, span)
	if q.node.TextGroup != "" {
		logical = partSpan(q.node.ID, span)
	}
	return uiaRangeQuery{uiaQuery: q, rng: r, text: text, span: span, logical: logical}, uiaOK
}

func (r *uiaTextRange) ranges() (inputtext.Range, TextRange) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.span, r.logical
}

func (r *uiaTextRange) storeLogical(span TextRange) {
	r.mu.Lock()
	r.logical = span
	r.mu.Unlock()
}

func (q uiaQuery) newLogical(span TextRange) *uiaTextRange {
	r := newUIATextRange(q.element, inputtext.Range{})
	r.logical = span
	return r
}

func (q uiaQuery) handOutLogical(out *uintptr, span TextRange) uintptr {
	r := q.newLogical(span)
	return uiaHandOut(out, r, &r.iface)
}

func (q uiaQuery) logicalArray(out *uintptr, span TextRange) uintptr {
	if span.Start.Node == "" {
		return uiaObjectArray(nil, out)
	}
	r := q.newLogical(span)
	return uiaObjectArray([]*uiaInterface{&r.iface}, out)
}

func (q uiaQuery) visibleRanges(out *uintptr) uintptr {
	var visible []*uiaInterface
	for _, id := range q.textView().parts() {
		n := q.snapshot.nodes[id]
		if !n.Bounds.Empty() && n.TextState == TextReady {
			r := q.newLogical(partRange(id))
			visible = append(visible, &r.iface)
		}
	}
	return uiaObjectArray(visible, out)
}

func (q uiaQuery) groupPoint(at image.Point) TextPoint {
	v := q.textView()
	for _, id := range v.parts() {
		n := q.snapshot.nodes[id]
		if at.In(n.Bounds) {
			return TextPoint{Node: id, Byte: uiaPointOffset(n, at)}
		}
	}
	return v.whole().Start
}

func (rq uiaRangeQuery) peerLogical(p uintptr) (TextRange, uintptr) {
	r, _ := uiaLookup(p).(*uiaTextRange)
	if r == nil || r.element.bridge != rq.element.bridge || r.element.gone.Load() {
		return TextRange{}, uiaInvalidArg
	}
	n, ok := rq.snapshot.Node(r.element.id)
	if !ok || n.Role != r.element.role {
		return TextRange{}, uiaUnavailable
	}
	peer := uiaQuery{element: r.element, snapshot: rq.snapshot, node: n, tree: rq.tree}
	if peer.textView().root != rq.textView().root {
		return TextRange{}, uiaInvalidArg
	}
	query, hr := peer.rangeQuery(r)
	return query.logical, hr
}

func (rq uiaRangeQuery) comparePoint(endpoint int32, other uintptr, target int32) (int, uintptr) {
	peer, hr := rq.peerLogical(other)
	if hr != uiaOK {
		return 0, hr
	}
	order, err := rq.textView().compare(rq.logical.endpoint(endpoint), peer.endpoint(target))
	return order, uiaRangeStatus(err)
}

func (rq uiaRangeQuery) compareRange(other uintptr) (bool, uintptr) {
	peer, hr := rq.peerLogical(other)
	if hr != uiaOK {
		return false, hr
	}
	view := rq.textView()
	first, err := view.compare(rq.logical.Start, peer.Start)
	if err != nil || first != 0 {
		return false, uiaRangeStatus(err)
	}
	last, err := view.compare(rq.logical.End, peer.End)
	return last == 0, uiaRangeStatus(err)
}

func (rq uiaRangeQuery) moveToRange(endpoint int32, other uintptr, target int32) uintptr {
	peer, hr := rq.peerLogical(other)
	if hr != uiaOK {
		return hr
	}
	return rq.storeEndpoint(endpoint, peer.endpoint(target))
}

func (rq uiaRangeQuery) storeEndpoint(endpoint int32, point TextPoint) uintptr {
	span := rq.logical
	if endpoint == uiaEndpointStart {
		span.Start = point
	} else {
		span.End = point
	}
	order, err := rq.textView().compare(span.Start, span.End)
	if err != nil {
		return uiaRangeStatus(err)
	}
	if order > 0 {
		span = TextRange{Start: point, End: point}
	}
	rq.rng.storeLogical(span)
	return uiaOK
}

func (rq uiaRangeQuery) rangeText(maxLength int) (string, uintptr) {
	if maxLength == 0 {
		return "", uiaOK
	}
	if rq.logical.Start.Node == "" {
		return uiaRangeText(rq.text, rq.span, maxLength), uiaOK
	}
	text, err := rq.textView().readUTF16(rq.logical, maxLength)
	return text, uiaRangeStatus(err)
}

func (rq uiaRangeQuery) boxes() []image.Rectangle {
	if rq.logical.Start.Node != "" {
		return rq.textView().rangeBoxes(rq.logical)
	}
	return uiaVisibleBoxes(rq.node, rq.span)
}

func uiaRangeStatus(err error) uintptr {
	switch {
	case err == nil:
		return uiaOK
	case errors.Is(err, ErrAccessibilityBusy):
		return uiaRetryLater
	case errors.Is(err, ErrElementUnavailable):
		return uiaUnavailable
	case errors.Is(err, inputtext.InvalidOffset):
		return uiaInvalidArg
	default:
		return uiaFail
	}
}

func partRange(id NodeID) TextRange {
	return TextRange{Start: TextPoint{Node: id}, End: TextPoint{Node: id, End: true}}
}

func partSpan(id NodeID, span inputtext.Range) TextRange {
	return TextRange{Start: TextPoint{Node: id, Byte: span.Start}, End: TextPoint{Node: id, Byte: span.End}}
}

func (r TextRange) endpoint(endpoint int32) TextPoint {
	if endpoint == uiaEndpointEnd {
		return r.End
	}
	return r.Start
}
