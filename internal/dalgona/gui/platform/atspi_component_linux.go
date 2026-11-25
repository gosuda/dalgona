//go:build linux

package platform

import (
	"image"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	atspiCoordScreen uint32 = 0
	atspiCoordWindow uint32 = 1
	atspiCoordParent uint32 = 2
	atspiActionName         = "activate"
)

type atspiRect struct {
	X, Y, Width, Height int32
}

var atspiComponentMembers = map[string]atspiMember{
	"Contains":             {in: []any{int32(0), int32(0), uint32(0)}, out: []any{false}, call: atspiContains},
	"GetAccessibleAtPoint": {in: []any{int32(0), int32(0), uint32(0)}, out: []any{atspiRef{}}, call: atspiAccessibleAtPoint},
	"GetExtents":           {in: []any{uint32(0)}, out: []any{atspiRect{}}, call: atspiExtents},
	"GetPosition":          {in: []any{uint32(0)}, out: []any{int32(0), int32(0)}, call: atspiPosition},
	"GetSize": {out: []any{int32(0), int32(0)}, call: func(o *atspiObject, _ []any) ([]any, error) {
		return []any{clampInt32(o.node.Bounds.Dx()), clampInt32(o.node.Bounds.Dy())}, nil
	}},
	"GrabFocus": {out: []any{false}, call: func(o *atspiObject, _ []any) ([]any, error) {
		return o.request(ActionFocus, "", textinput.Range{})
	}},
	"ScrollTo": {in: []any{uint32(0)}, out: []any{false}, call: func(o *atspiObject, _ []any) ([]any, error) {
		return o.request(ActionScrollIntoView, "", textinput.Range{})
	}},
}

func (o *atspiObject) parentOrigin() image.Point {
	switch {
	case o.kind != atspiNodeKind:
		return image.Point{}
	case o.node.Parent == "":
		return o.node.Bounds.Min
	}
	parent, _ := o.snap.Node(o.node.Parent)
	return parent.Bounds.Min
}

func (o *atspiObject) offset(coord uint32) (image.Point, error) {
	switch coord {
	case atspiCoordScreen:
		return o.bridge.screenOrigin(), nil
	case atspiCoordWindow:
		return image.Point{}, nil
	case atspiCoordParent:
		return image.Point{}.Sub(o.parentOrigin()), nil
	}
	return image.Point{}, atspiInvalid("coordinate type %d is unknown; use 0 for screen, 1 for window, or 2 for parent coordinates", coord)
}

func (o *atspiObject) point(args []any) (image.Point, error) {
	offset, err := o.offset(args[2].(uint32))
	if err != nil {
		return image.Point{}, err
	}
	return image.Pt(int(args[0].(int32)), int(args[1].(int32))).Sub(offset), nil
}

func atspiContains(o *atspiObject, args []any) ([]any, error) {
	point, err := o.point(args)
	if err != nil {
		return nil, err
	}
	return []any{point.In(o.node.Bounds)}, nil
}

func atspiAccessibleAtPoint(o *atspiObject, args []any) ([]any, error) {
	point, err := o.point(args)
	if err != nil {
		return nil, err
	}
	if hit := atspiChildHit(o.snap, o.childNodes(), point); hit != "" {
		return []any{o.ref(hit)}, nil
	}
	return []any{atspiRef{Path: atspiNullPath}}, nil
}

func atspiChildHit(s *SemanticSnapshot, children []NodeID, point image.Point) NodeID {
	for i := len(children) - 1; i >= 0; i-- {
		child, _ := s.Node(children[i])
		if point.In(child.Bounds) {
			return child.ID
		}
		if child.Bounds.Empty() {
			if hit := atspiChildHit(s, s.Children(child.ID), point); hit != "" {
				return hit
			}
		}
	}
	return ""
}

func atspiExtents(o *atspiObject, args []any) ([]any, error) {
	offset, err := o.offset(args[0].(uint32))
	if err != nil {
		return nil, err
	}
	r := o.node.Bounds.Add(offset)
	return []any{atspiRect{X: clampInt32(r.Min.X), Y: clampInt32(r.Min.Y), Width: clampInt32(r.Dx()), Height: clampInt32(r.Dy())}}, nil
}

func atspiPosition(o *atspiObject, args []any) ([]any, error) {
	offset, err := o.offset(args[0].(uint32))
	if err != nil {
		return nil, err
	}
	p := o.node.Bounds.Min.Add(offset)
	return []any{clampInt32(p.X), clampInt32(p.Y)}, nil
}

type atspiActionInfo struct {
	Name, Description, KeyBinding string
}

var atspiActionProperties = map[string]atspiProperty{
	"version":  atspiConstant(uint32(1)),
	"NActions": atspiConstant(int32(1)),
}

var atspiActionMembers = map[string]atspiMember{
	"GetDescription":   {in: []any{int32(0)}, out: []any{""}, call: atspiActionField(func(n SemanticNode) string { return n.Action })},
	"GetName":          {in: []any{int32(0)}, out: []any{""}, call: atspiActionField(func(SemanticNode) string { return atspiActionName })},
	"GetLocalizedName": {in: []any{int32(0)}, out: []any{""}, call: atspiActionField(func(n SemanticNode) string { return n.Action })},
	"GetKeyBinding":    {in: []any{int32(0)}, out: []any{""}, call: atspiActionField(func(SemanticNode) string { return "" })},
	"GetActions": {out: []any{[]atspiActionInfo{}}, call: atspiAnswer(func(o *atspiObject) any {
		return []atspiActionInfo{{Name: o.node.Action, Description: o.node.Action}}
	})},
	"DoAction": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		if err := atspiActionIndex(args[0].(int32)); err != nil {
			return nil, err
		}
		return o.request(ActionActivate, "", textinput.Range{})
	}},
}

func atspiActionIndex(index int32) error {
	if index != 0 {
		return atspiInvalid("action %d does not exist because the object has one action; use index 0", index)
	}
	return nil
}

func atspiActionField(field func(SemanticNode) string) func(o *atspiObject, args []any) ([]any, error) {
	return func(o *atspiObject, args []any) ([]any, error) {
		if err := atspiActionIndex(args[0].(int32)); err != nil {
			return nil, err
		}
		return []any{field(o.node)}, nil
	}
}
