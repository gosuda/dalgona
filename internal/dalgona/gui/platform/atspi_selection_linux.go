//go:build linux

package platform

import "github.com/gosuda/dalgona/internal/dal/inputtext"

func atspiSelectable(parent, child Role) bool {
	return parent == RoleList && child == RoleListItem || parent == RoleMenu && child == RoleMenuItem || child == RoleRadioButton
}

func atspiSelectableIn(snap *SemanticSnapshot, n SemanticNode) bool {
	if n.Selectable {
		return true
	}
	if snap == nil {
		return false
	}
	parent, ok := snap.Node(n.Parent)
	return ok && atspiSelectable(parent.Role, n.Role)
}

func atspiSelectableChildren(snap *SemanticSnapshot, parent SemanticNode) []SemanticNode {
	var children []SemanticNode
	if snap == nil {
		return children
	}
	for _, id := range snap.Children(parent.ID) {
		if child, _ := snap.Node(id); child.Selectable || atspiSelectable(parent.Role, child.Role) {
			children = append(children, child)
		}
	}
	return children
}

func atspiSelectedChildren(snap *SemanticSnapshot, parent SemanticNode) []NodeID {
	var selected []NodeID
	for _, child := range atspiSelectableChildren(snap, parent) {
		if child.selected() {
			selected = append(selected, child.ID)
		}
	}
	return selected
}

var atspiSelectionProperties = map[string]atspiProperty{
	"version": atspiConstant(uint32(1)),
	"NSelectedChildren": atspiRead(func(o *atspiObject) any {
		return clampInt32(len(atspiSelectedChildren(o.snap, o.node)))
	}),
}

var atspiSelectionMembers = map[string]atspiMember{
	"GetSelectedChild": {in: []any{int32(0)}, out: []any{atspiRef{}}, call: func(o *atspiObject, args []any) ([]any, error) {
		child, ok := o.selectedChild(args[0].(int32))
		if !ok {
			return []any{atspiRef{Path: atspiNullPath}}, nil
		}
		return []any{child.self()}, nil
	}},
	"SelectChild": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		child, err := o.childAt(args[0].(int32))
		if err != nil {
			return nil, err
		}
		return child.choose(true)
	}},
	"DeselectChild": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		child, err := o.childAt(args[0].(int32))
		if err != nil {
			return nil, err
		}
		return child.choose(false)
	}},
	"DeselectSelectedChild": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		child, ok := o.selectedChild(args[0].(int32))
		if !ok {
			return []any{false}, nil
		}
		return child.choose(false)
	}},
	"IsChildSelected": {in: []any{int32(0)}, out: []any{false}, call: func(o *atspiObject, args []any) ([]any, error) {
		child, err := o.childAt(args[0].(int32))
		if err != nil {
			return nil, err
		}
		return []any{atspiSelectableIn(o.snap, child.node) && child.node.selected()}, nil
	}},
	"SelectAll":      {out: []any{false}, call: atspiAnswer(func(*atspiObject) any { return false })},
	"ClearSelection": {out: []any{false}, call: atspiAnswer(func(*atspiObject) any { return false })},
}

func (o *atspiObject) childAt(index int32) (*atspiObject, error) {
	children := o.childNodes()
	if index < 0 || int(index) >= len(children) {
		return nil, atspiInvalid("child %d does not exist because the object has %d children; ask GetChildren for the current list", index, len(children))
	}
	child, _ := o.snap.Node(children[index])
	return o.object(atspiNodeKind, child), nil
}

func (o *atspiObject) selectedChild(index int32) (*atspiObject, bool) {
	selected := atspiSelectedChildren(o.snap, o.node)
	if index < 0 || int(index) >= len(selected) {
		return nil, false
	}
	child, _ := o.snap.Node(selected[index])
	return o.object(atspiNodeKind, child), true
}

func (o *atspiObject) choose(selected bool) ([]any, error) {
	switch {
	case !atspiSelectableIn(o.snap, o.node):
		return []any{false}, nil
	case o.node.selected() == selected:
		return []any{true}, nil
	case !selected && o.node.Role == RoleRadioButton:
		return []any{false}, nil
	}
	return o.request(ActionActivate, "", inputtext.Range{})
}
