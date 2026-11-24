//go:build gui

package platform

import (
	"image"
	"strings"
	"testing"

	"gioui.org/io/input"
	"gioui.org/io/semantic"
	"gioui.org/op"
	"gioui.org/op/clip"
)

func describe(t *testing.T, nodes []SemanticNode) map[string]input.SemanticDesc {
	t.Helper()
	ops := new(op.Ops)
	for i, n := range nodes {
		area := clip.Rect(image.Rect(0, i*10, 10, i*10+10)).Push(ops)
		n.Add(ops)
		area.Pop()
	}
	var router input.Router
	router.Frame(ops)
	found := make(map[string]input.SemanticDesc, len(nodes))
	for _, s := range router.AppendSemantics(nil) {
		if s.Desc.Label != "" {
			found[s.Desc.Label] = s.Desc
		}
	}
	return found
}

func TestSemanticRolesFollowTheSpecifiedGioClassTable(t *testing.T) {
	table := map[semantic.ClassOp][]Role{
		semantic.Button:      {RoleButton, RoleLink, RoleMenuItem},
		semantic.CheckBox:    {RoleCheckBox},
		semantic.RadioButton: {RoleRadioButton},
		semantic.Switch:      {RoleSwitch},
		semantic.Editor:      {RoleTextField, RoleSecureField},
		semantic.Unknown: {
			RoleDocument, RoleHeading, RoleList, RoleListItem, RoleTable, RoleRow, RoleColumnHeader, RoleRowHeader, RoleCell,
			RoleCode, RoleMath, RoleFigure, RoleImage, RoleTerminal, RoleDialog, RoleMenu, RoleStatus, RoleError,
		},
	}
	var nodes []SemanticNode
	want := map[string]semantic.ClassOp{}
	for class, roles := range table {
		for _, role := range roles {
			name := class.String() + " role " + string(rune('A'+role))
			nodes = append(nodes, SemanticNode{ID: NodeID(name), Role: role, Name: name})
			want[name] = class
		}
	}
	if len(nodes) != int(RoleError) {
		t.Fatalf("the table covers %d roles, want all %d", len(nodes), RoleError)
	}
	got := describe(t, nodes)
	for name, class := range want {
		desc, ok := got[name]
		if !ok {
			t.Fatalf("no Gio semantic node for %q", name)
		}
		if desc.Class != class {
			t.Errorf("%q: Gio class %v, want %v", name, desc.Class, class)
		}
	}
}

func TestSemanticStatesReachGio(t *testing.T) {
	got := describe(t, []SemanticNode{
		{ID: "on", Role: RoleSwitch, Name: "Notifications", Action: "Toggle", Checked: true},
		{ID: "off", Role: RoleCheckBox, Name: "Wrap lines", Action: "Toggle"},
		{ID: "send", Role: RoleButton, Name: "Send", Action: "Send", Description: "Sends the draft", Disabled: true, Reason: "Connect to a daemon first"},
		{ID: "token", Role: RoleSecureField, Name: "API token", Action: "Edit", Description: "Protected"},
	})
	if d := got["Notifications"]; !d.Selected || d.Disabled {
		t.Errorf("checked switch: selected %v disabled %v", d.Selected, d.Disabled)
	}
	if d := got["Wrap lines"]; d.Selected {
		t.Error("an unchecked box reports selected")
	}
	send := got["Send"]
	if !send.Disabled || !strings.Contains(send.Description, "Connect to a daemon first") || !strings.Contains(send.Description, "Sends the draft") {
		t.Errorf("disabled action lost its state or reason: %+v", send)
	}
	if token := got["API token"]; token.Class != semantic.Editor || token.Disabled {
		t.Errorf("secure field: %+v", token)
	}
}
