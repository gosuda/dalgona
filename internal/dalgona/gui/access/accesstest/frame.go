//go:build gui

package accesstest

import (
	"image"
	"testing"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type Frame struct {
	Tree     *platform.AccessibilityTree
	Wakeups  int
	registry access.Frame
	Nodes    []platform.SemanticNode
}

func New() *Frame {
	f := new(Frame)
	f.Tree = platform.NewAccessibilityTree(8, func() { f.Wakeups++ })
	return f
}

func (f *Frame) Begin(gtx layout.Context) layout.Context {
	if f == nil {
		return gtx
	}
	return f.registry.Begin(gtx, f.Tree, image.Rectangle{Max: gtx.Constraints.Max})
}

func (f *Frame) End(t testing.TB, gtx layout.Context) {
	t.Helper()
	if f == nil {
		return
	}
	if err := f.registry.Publish(gtx, f.Tree, f.registry.Focus(gtx)); err != nil {
		t.Fatal(err)
	}
	s := f.Tree.Snapshot()
	f.Nodes = descendants(s, s.Root(), f.Nodes[:0])
	if err := platform.CheckSemantics(f.Nodes); err != nil {
		t.Fatal(err)
	}
}

func descendants(s *platform.SemanticSnapshot, id platform.NodeID, nodes []platform.SemanticNode) []platform.SemanticNode {
	n, _ := s.Node(id)
	nodes = append(nodes, n)
	for _, child := range s.Children(id) {
		nodes = descendants(s, child, nodes)
	}
	return nodes
}

func (f *Frame) Find(t testing.TB, name string, role platform.Role) platform.SemanticNode {
	t.Helper()
	for _, n := range f.Nodes {
		if n.Name == name && n.Role == role {
			return n
		}
	}
	t.Fatalf("no accessible node %q with role %d", name, role)
	return platform.SemanticNode{}
}

func (f *Frame) Request(t testing.TB, n platform.SemanticNode, action platform.AccessibilityAction) {
	t.Helper()
	if err := f.Tree.Request(platform.AccessibilityIntent{Node: n.ID, Revision: f.Tree.Snapshot().Revision(), Action: action}); err != nil {
		t.Fatal(err)
	}
}
