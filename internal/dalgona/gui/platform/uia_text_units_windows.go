//go:build windows && (amd64 || arm64)

package platform

import "github.com/gosuda/dalgona/internal/dal/textinput"

type uiaSegment struct {
	point TextPoint
	base  int
	at    int
	text  string
}

func (v textView) uiaSegment(p TextPoint) (uiaSegment, error) {
	if p.Byte < 0 {
		n, err := v.part(p.Node)
		return uiaBefore(n, len(n.TextBefore)+p.Byte), err
	}
	n, err := v.load(p.Node)
	return uiaSegment{point: p, at: uiaSnap(n.Value, p.offset(n)), text: n.Value}, err
}

func uiaBefore(n SemanticNode, at int) uiaSegment {
	return uiaSegment{point: TextPoint{Node: n.ID}, at: at, base: -len(n.TextBefore), text: n.TextBefore}
}

func (s uiaSegment) atByte(at int) TextPoint {
	return TextPoint{Node: s.point.Node, Byte: s.base + at}
}

func (v textView) uiaNext(p TextPoint, unit uiaTextUnit) (TextPoint, error) {
	for {
		s, err := v.uiaSegment(p)
		if err != nil {
			return p, err
		}
		if s.at < len(s.text) {
			return s.atByte(unit.next(s.text, s.at)), nil
		}
		next := v.snap.textOrder[p.Node] + 1
		if next == len(v.parts()) {
			return p, nil
		}
		n := v.snap.nodes[v.parts()[next]]
		p = TextPoint{Node: n.ID, Byte: -len(n.TextBefore)}
	}
}

func (v textView) uiaPrevious(p TextPoint, unit uiaTextUnit) (TextPoint, error) {
	for {
		s, err := v.uiaSegment(p)
		if err != nil {
			return p, err
		}
		if s.at == 0 && s.base == 0 {
			n := v.snap.nodes[p.Node]
			s = uiaBefore(n, len(n.TextBefore))
		}
		if s.at > 0 {
			return s.atByte(unit.previous(s.text, s.at)), nil
		}
		prior := v.snap.textOrder[p.Node] - 1
		if prior < 0 {
			return p, nil
		}
		p = TextPoint{Node: v.parts()[prior], End: true}
	}
}

func (v textView) uiaWalk(p TextPoint, unit uiaTextUnit, count int32) (TextPoint, int32, error) {
	if unit == uiaByDocument && count != 0 {
		return v.uiaDocumentStep(p, count)
	}
	step, stride := v.uiaNext, int32(1)
	if count < 0 {
		step, stride = v.uiaPrevious, -1
	}
	var moved int32
	for moved != count {
		next, err := step(p, unit)
		if err != nil {
			return p, moved, err
		}
		if next == p {
			break
		}
		p, moved = next, moved+stride
	}
	return p, moved, nil
}

func (v textView) uiaDocumentStep(p TextPoint, count int32) (TextPoint, int32, error) {
	to, moved := v.whole().End, int32(1)
	if count < 0 {
		to, moved = v.whole().Start, -1
	}
	order, err := v.compare(p, to)
	if err != nil || order == 0 {
		return p, 0, err
	}
	return to, moved, nil
}

func (v textView) uiaExpand(p TextPoint, unit uiaTextUnit) (TextRange, error) {
	if unit == uiaByDocument {
		return v.whole(), nil
	}
	s, err := v.uiaSegment(p)
	if err != nil {
		return TextRange{}, err
	}
	from := s.atByte(unit.floor(s.text, s.at))
	to, err := v.uiaNext(from, unit)
	return TextRange{Start: from, End: to}, err
}

func (rq uiaRangeQuery) move(unit uiaTextUnit, count int32, out *int32) uintptr {
	view := rq.textView()
	p, moved, err := view.uiaWalk(rq.logical.Start, unit, count)
	if err != nil || moved == 0 {
		return uiaRangeStatus(err)
	}
	span := TextRange{Start: p, End: p}
	if rq.logical.Start != rq.logical.End {
		span, err = view.uiaExpand(p, unit)
		if err != nil {
			return uiaRangeStatus(err)
		}
	}
	rq.rng.storeLogical(span)
	*out = moved
	return uiaOK
}

func (rq uiaRangeQuery) moveEndpoint(endpoint int32, unit uiaTextUnit, count int32, out *int32) uintptr {
	p, moved, err := rq.textView().uiaWalk(rq.logical.endpoint(endpoint), unit, count)
	if err != nil || moved == 0 {
		return uiaRangeStatus(err)
	}
	if hr := rq.storeEndpoint(endpoint, p); hr != uiaOK {
		return hr
	}
	*out = moved
	return uiaOK
}

func (rq uiaRangeQuery) findLogical(needle string, backward, fold bool, out *uintptr) uintptr {
	view := rq.textView()
	text, err := view.read(rq.logical)
	if err != nil {
		return uiaRangeStatus(err)
	}
	found, ok := uiaFind(text, textinput.Range{End: len(text)}, needle, backward, fold)
	if !ok {
		return uiaOK
	}
	base, err := view.offset(rq.logical.Start, textinput.Bytes)
	if err != nil {
		return uiaRangeStatus(err)
	}
	span, err := view.unitsRange(base+found.Start, base+found.End, textinput.Bytes)
	if err != nil {
		return uiaRangeStatus(err)
	}
	return rq.handOutLogical(out, span)
}
