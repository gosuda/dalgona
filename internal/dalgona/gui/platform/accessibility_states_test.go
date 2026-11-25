package platform

import "testing"

func TestUIAExpandableAndSelectableProperties(t *testing.T) {
	n := SemanticNode{Role: RoleButton, Selectable: true, Selected: true, Expandable: true, Expanded: true}
	for _, c := range []struct {
		pattern, property int32
		value             uiaValue
	}{{10010, 30079, uiaFlag(true)}, {10005, 30070, uiaNumber(1)}} {
		if !uiaSupportsPattern(n, c.pattern) || !uiaProperty(nil, n, c.property).same(c.value) {
			t.Errorf("button does not offer UIA pattern %d and property %d", c.pattern, c.property)
		}
	}
	n.Expanded = false
	if got := uiaProperty(nil, n, 30070); !got.same(uiaNumber(0)) {
		t.Errorf("a closed menu reports %+v, want collapsed", got)
	}
}

func TestUIAPlaceholderIsHelpText(t *testing.T) {
	n := SemanticNode{Role: RoleTextField, Placeholder: "Write a message", Description: "Shift+Enter starts a new line"}
	if got := uiaProperty(nil, n, 30013); !got.same(uiaText("Shift+Enter starts a new line; Write a message")) {
		t.Errorf("help text lost the placeholder: %+v", got)
	}
}

func TestAppKitPublishesDisclosureSelectionAndPlaceholder(t *testing.T) {
	nodes := []SemanticNode{
		{ID: "root", Role: RoleDocument},
		{ID: "menu", Parent: "root", Role: RoleButton, Name: "Menu", Action: "activate", Expandable: true, Expanded: true},
		{ID: "thread", Parent: "root", Role: RoleButton, Name: "Thread", Action: "select", Selectable: true, Selected: true},
		{ID: "composer", Parent: "root", Role: RoleTextField, Name: "Message", Action: "edit", Placeholder: "Write a message"},
	}
	got := encodeWire(t, nodes)
	if got[1].states != 12 || got[2].states != 3 || got[3].placeholder != "Write a message" {
		t.Fatalf("missing AppKit control data: disclosure %#x selection %#x placeholder %q", got[1].states, got[2].states, got[3].placeholder)
	}
}
