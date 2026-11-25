package platform

import (
	"fmt"
	"image"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func uiaTestSnapshot(t *testing.T, revision uint64, focus NodeID, nodes ...SemanticNode) *SemanticSnapshot {
	t.Helper()
	s, err := NewSemanticSnapshot(revision, focus, nodes)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func TestUIAControlTypesUseMicrosoftLearnIdentifiers(t *testing.T) {
	want := map[Role]int32{
		RoleButton:       50000,
		RoleCheckBox:     50002,
		RoleRadioButton:  50013,
		RoleSwitch:       50000,
		RoleTextField:    50004,
		RoleSecureField:  50004,
		RoleLink:         50005,
		RoleMenuItem:     50011,
		RoleDocument:     50030,
		RoleHeading:      50020,
		RoleList:         50008,
		RoleListItem:     50007,
		RoleTable:        50036,
		RoleRow:          50029,
		RoleColumnHeader: 50035,
		RoleRowHeader:    50035,
		RoleCell:         50029,
		RoleCode:         50020,
		RoleMath:         50020,
		RoleFigure:       50026,
		RoleImage:        50006,
		RoleTerminal:     50030,
		RoleDialog:       50033,
		RoleMenu:         50009,
		RoleStatus:       50017,
		RoleError:        50020,
	}
	for r := RoleButton; r <= RoleError; r++ {
		if got := uiaControlType(r); got != want[r] {
			t.Errorf("role %d maps to control type %d, want %d", r, got, want[r])
		}
		got := uiaProperty(nil, SemanticNode{ID: "n", Role: r}, 30003)
		if got.kind != uiaInt || got.number != want[r] {
			t.Errorf("role %d reports UIA_ControlTypePropertyId %+v, want %d", r, got, want[r])
		}
	}
	if got := uiaControlType(RoleError + 1); got != 0 {
		t.Errorf("an unknown role maps to control type %d", got)
	}
}

func TestUIAPatternsFollowMicrosoftLearnControlTypes(t *testing.T) {
	cases := []struct {
		role    Role
		pattern int32
		want    bool
	}{
		{RoleButton, 10000, true},
		{RoleButton, 10015, false},
		{RoleSwitch, 10015, true},
		{RoleSwitch, 10000, false},
		{RoleCheckBox, 10015, true},
		{RoleRadioButton, 10010, true},
		{RoleRadioButton, 10015, false},
		{RoleLink, 10000, true},
		{RoleMenuItem, 10000, true},
		{RoleTextField, 10002, true},
		{RoleTextField, 10014, true},
		{RoleSecureField, 10002, true},
		{RoleSecureField, 10014, false},
		{RoleDocument, 10014, false},
		{RoleTerminal, 10014, true},
		{RoleHeading, 10002, false},
		{RoleError, 10002, false},
		{RoleImage, 10000, false},
		{RoleImage, 10010, false},
		{RoleTable, 10006, true},
		{RoleTable, 10012, true},
		{RoleCell, 10007, true},
		{RoleCell, 10013, true},
		{RoleColumnHeader, 10007, true},
		{RoleColumnHeader, 10013, true},
		{RoleRowHeader, 10007, true},
		{RoleRowHeader, 10013, true},
		{RoleRow, 10007, false},
	}
	for _, c := range cases {
		if got := uiaSupportsPattern(SemanticNode{Role: c.role}, c.pattern); got != c.want {
			t.Errorf("role %d pattern %d: supported %t, want %t", c.role, c.pattern, got, c.want)
		}
	}
	for _, n := range []SemanticNode{{Role: RoleDocument, Value: "Text"}, {Role: RoleDocument, TextState: TextUncached}} {
		if !uiaSupportsPattern(n, uiaTextPattern) {
			t.Errorf("document range %q in state %d does not offer TextPattern", n.Value, n.TextState)
		}
	}
	for r := RoleButton; r <= RoleError; r++ {
		if !uiaSupportsPattern(SemanticNode{Role: r}, 10017) {
			t.Errorf("role %d cannot be scrolled into view through ScrollItem", r)
		}
		if uiaSupportsPattern(SemanticNode{Role: r}, 10003) {
			t.Errorf("role %d claims RangeValue without a range model", r)
		}
	}
	if uiaSupportsPattern(SemanticNode{}, 10017) {
		t.Error("an unknown role claims a control pattern")
	}
}

func uiaPropertyTree(t *testing.T) *SemanticSnapshot {
	t.Helper()
	return uiaTestSnapshot(t, 3, "name",
		SemanticNode{ID: "doc", Role: RoleDocument, Name: "Transcript", Bounds: image.Rect(0, 0, 800, 600)},
		SemanticNode{ID: "plan", Parent: "doc", Role: RoleHeading, Name: "Plan", Level: 2},
		SemanticNode{ID: "name", Parent: "doc", Role: RoleTextField, Name: "Name", Action: "Edit", Value: "Ada", Description: "Display name", Bounds: image.Rect(10, 10, 200, 40)},
		SemanticNode{ID: "token", Parent: "doc", Role: RoleSecureField, Name: "Token", Action: "Edit"},
		SemanticNode{ID: "token-error", Parent: "token", Role: RoleError, Name: "The token is empty", Target: "token"},
		SemanticNode{ID: "send", Parent: "doc", Role: RoleButton, Name: "Send", Action: "Send", Description: "Sends the draft", Disabled: true, Reason: "Connect first"},
		SemanticNode{ID: "steps", Parent: "doc", Role: RoleList, Name: "Steps"},
		SemanticNode{ID: "steps-icon", Parent: "steps", Role: RoleImage, Name: "Checklist"},
		SemanticNode{ID: "step-1", Parent: "steps", Role: RoleListItem, Name: "One"},
		SemanticNode{ID: "step-2", Parent: "steps", Role: RoleListItem, Name: "Two"},
		SemanticNode{ID: "step-3", Parent: "steps", Role: RoleListItem, Name: "Three"},
		SemanticNode{ID: "confirm", Parent: "doc", Role: RoleDialog, Name: "Confirm"},
	)
}

func TestUIAPropertiesExposeNodeState(t *testing.T) {
	s := uiaPropertyTree(t)
	cases := []struct {
		node     NodeID
		property int32
		want     uiaValue
	}{
		{"send", 30005, uiaText("Send")},
		{"send", 30011, uiaText("send")},
		{"send", 30013, uiaText("Sends the draft; Connect first")},
		{"name", 30013, uiaText("Display name")},
		{"plan", 30013, uiaValue{}},
		{"send", 30010, uiaFlag(false)},
		{"name", 30010, uiaFlag(true)},
		{"name", 30009, uiaFlag(true)},
		{"send", 30009, uiaFlag(false)},
		{"plan", 30009, uiaFlag(false)},
		{"name", 30008, uiaFlag(true)},
		{"send", 30008, uiaFlag(false)},
		{"token", 30019, uiaFlag(true)},
		{"name", 30019, uiaFlag(false)},
		{"plan", 30173, uiaNumber(80052)},
		{"name", 30173, uiaValue{}},
		{"confirm", 30174, uiaFlag(true)},
		{"name", 30174, uiaValue{}},
		{"token", 30105, uiaValue{kind: uiaElements, nodes: []NodeID{"token-error"}}},
		{"name", 30105, uiaValue{}},
		{"token", 30103, uiaFlag(false)},
		{"name", 30103, uiaValue{}},
		{"step-2", 30152, uiaNumber(2)},
		{"step-2", 30153, uiaNumber(3)},
		{"plan", 30152, uiaValue{}},
		{"plan", 30022, uiaFlag(true)},
		{"name", 30022, uiaFlag(false)},
		{"name", 30001, uiaValue{}},
	}
	for _, c := range cases {
		n, _ := s.Node(c.node)
		if got := uiaProperty(s, n, c.property); !got.same(c.want) {
			t.Errorf("%s property %d = %+v, want %+v", c.node, c.property, got, c.want)
		}
	}
}

func TestUIASecureFieldValueNeverLeaks(t *testing.T) {
	if got := uiaValueText(SemanticNode{Role: RoleSecureField, Value: "sk-live"}); got != "" {
		t.Fatalf("a secure field reported the value %q", got)
	}
	s := uiaTestSnapshot(t, 1, "", SemanticNode{ID: "token", Role: RoleSecureField, Name: "Token", Action: "Edit"})
	before := SemanticNode{ID: "token", Role: RoleSecureField, Name: "Token", Action: "Edit", Value: "sk-old"}
	after := SemanticNode{ID: "token", Role: RoleSecureField, Name: "Token", Action: "Edit", Value: "sk-new"}
	for _, e := range uiaNodeChanges(nil, s, s, before, after) {
		if e.previous.text != "" || e.next.text != "" {
			t.Fatalf("a secure field change carried text %+v", e)
		}
	}
}

func TestUIARuntimeIDsAppendToTheHostWindow(t *testing.T) {
	if got, want := uiaRuntimeID("a"), [3]int32{3, 1471278630, 100789388}; got != want {
		t.Fatalf("runtime id of %q = %v, want %v from the FNV-1a 64 vector 0xaf63dc4c8601ec8c", "a", got, want)
	}
	seen := map[[3]int32]NodeID{}
	for _, id := range []NodeID{"a", "b", "block-1", "block-2", "block-10", "transcript/turn-7/item-3"} {
		got := uiaRuntimeID(id)
		if got[0] != 3 {
			t.Errorf("runtime id of %q does not start with UiaAppendRuntimeId: %v", id, got)
		}
		if got != uiaRuntimeID(id) {
			t.Errorf("runtime id of %q changed between calls", id)
		}
		if other, dup := seen[got]; dup {
			t.Errorf("runtime ids of %q and %q collide", id, other)
		}
		seen[got] = id
	}
}

func TestUIANavigationWalksTheTreeInReadingOrder(t *testing.T) {
	s := uiaPropertyTree(t)
	cases := []struct {
		from      NodeID
		direction int32
		want      NodeID
		window    bool
	}{
		{"", 3, "doc", false},
		{"", 4, "doc", false},
		{"", 0, "", false},
		{"", 1, "", false},
		{"doc", 0, "", true},
		{"doc", 1, "", false},
		{"doc", 3, "plan", false},
		{"doc", 4, "confirm", false},
		{"name", 1, "token", false},
		{"name", 2, "plan", false},
		{"plan", 2, "", false},
		{"confirm", 1, "", false},
		{"token-error", 0, "token", false},
		{"token-error", 3, "", false},
		{"step-2", 0, "steps", false},
		{"missing", 0, "", false},
		{"doc", 7, "", false},
	}
	for _, c := range cases {
		got, window := uiaNavigation(s, c.from, c.direction)
		if got != c.want || window != c.window {
			t.Errorf("navigate %q direction %d = (%q, window %t), want (%q, window %t)", c.from, c.direction, got, window, c.want, c.window)
		}
	}
	if got, window := uiaNavigation(nil, "", 3); got != "" || window {
		t.Errorf("an empty tree navigated to %q", got)
	}
}

func TestUIAHitTestReturnsTheTopmostDeepestNode(t *testing.T) {
	s := uiaTestSnapshot(t, 1, "",
		SemanticNode{ID: "doc", Role: RoleDocument, Bounds: image.Rect(0, 0, 400, 400)},
		SemanticNode{ID: "list", Parent: "doc", Role: RoleList},
		SemanticNode{ID: "item", Parent: "list", Role: RoleListItem, Bounds: image.Rect(10, 10, 110, 60)},
		SemanticNode{ID: "under", Parent: "doc", Role: RoleImage, Bounds: image.Rect(200, 200, 300, 300)},
		SemanticNode{ID: "over", Parent: "doc", Role: RoleButton, Name: "Close", Action: "Close", Bounds: image.Rect(250, 250, 350, 350)},
	)
	cases := []struct {
		point image.Point
		want  NodeID
	}{
		{image.Pt(20, 20), "item"},
		{image.Pt(260, 260), "over"},
		{image.Pt(210, 210), "under"},
		{image.Pt(390, 10), "doc"},
		{image.Pt(500, 500), ""},
		{image.Pt(110, 20), "doc"},
	}
	for _, c := range cases {
		if got := uiaHitTest(s, c.point); got != c.want {
			t.Errorf("hit test %v = %q, want %q", c.point, got, c.want)
		}
	}
	if got := uiaHitTest(nil, image.Pt(1, 1)); got != "" {
		t.Errorf("an empty tree hit %q", got)
	}
}

func uiaEventText(e uiaEvent) string {
	return fmt.Sprintf("%d %s %d %+v %+v", e.kind, e.node, e.id, e.previous, e.next)
}

func uiaEventTexts(events []uiaEvent) []string {
	texts := make([]string, 0, len(events))
	for _, e := range events {
		texts = append(texts, uiaEventText(e))
	}
	return texts
}

func TestUIAChangesRaiseMicrosoftLearnEvents(t *testing.T) {
	base := []SemanticNode{
		{ID: "doc", Role: RoleDocument, Name: "Transcript"},
		{ID: "name", Parent: "doc", Role: RoleTextField, Name: "Name", Action: "Edit", Value: "Ada"},
		{ID: "agree", Parent: "doc", Role: RoleCheckBox, Name: "Agree", Action: "Toggle"},
		{ID: "fast", Parent: "doc", Role: RoleRadioButton, Name: "Fast", Action: "Select", Checked: true},
		{ID: "slow", Parent: "doc", Role: RoleRadioButton, Name: "Slow", Action: "Select"},
		{ID: "send", Parent: "doc", Role: RoleButton, Name: "Send", Action: "Send"},
	}
	edit := func(change func(nodes []SemanticNode) []SemanticNode) []SemanticNode {
		return change(slices.Clone(base))
	}
	previous := uiaTestSnapshot(t, 1, "name", base...)
	cases := []struct {
		name  string
		next  []SemanticNode
		focus NodeID
		want  []uiaEvent
	}{
		{"name", edit(func(n []SemanticNode) []SemanticNode { n[5].Name = "Send now"; return n }), "name", []uiaEvent{
			{kind: uiaPropertyChange, node: "send", id: 30005, previous: uiaText("Send"), next: uiaText("Send now")},
		}},
		{"value", edit(func(n []SemanticNode) []SemanticNode { n[1].Value = "Ada L"; return n }), "name", []uiaEvent{
			{kind: uiaPropertyChange, node: "name", id: 30045, previous: uiaText("Ada"), next: uiaText("Ada L")},
			{kind: uiaRaise, node: "name", id: 20015},
		}},
		{"selection", edit(func(n []SemanticNode) []SemanticNode { n[1].Selection = textinput.Range{Start: 1, End: 3}; return n }), "name", []uiaEvent{
			{kind: uiaRaise, node: "name", id: 20014},
		}},
		{"toggle", edit(func(n []SemanticNode) []SemanticNode { n[2].Checked = true; return n }), "name", []uiaEvent{
			{kind: uiaPropertyChange, node: "agree", id: 30086, previous: uiaNumber(0), next: uiaNumber(1)},
		}},
		{"radio", edit(func(n []SemanticNode) []SemanticNode { n[3].Checked, n[4].Checked = false, true; return n }), "name", []uiaEvent{
			{kind: uiaPropertyChange, node: "fast", id: 30079, previous: uiaFlag(true), next: uiaFlag(false)},
			{kind: uiaRaise, node: "fast", id: 20011},
			{kind: uiaPropertyChange, node: "slow", id: 30079, previous: uiaFlag(false), next: uiaFlag(true)},
			{kind: uiaRaise, node: "slow", id: 20012},
		}},
		{"disable", edit(func(n []SemanticNode) []SemanticNode { n[5].Disabled, n[5].Reason = true, "Offline"; return n }), "name", []uiaEvent{
			{kind: uiaPropertyChange, node: "send", id: 30013, previous: uiaText(""), next: uiaText("Offline")},
			{kind: uiaPropertyChange, node: "send", id: 30010, previous: uiaFlag(true), next: uiaFlag(false)},
		}},
		{"focus", base, "send", []uiaEvent{
			{kind: uiaRaise, node: "send", id: 20005},
		}},
		{"remove", edit(func(n []SemanticNode) []SemanticNode { return n[:5] }), "name", []uiaEvent{
			{kind: uiaRetire, node: "send"},
			{kind: uiaChildrenInvalidated, node: "doc"},
		}},
		{"add", append(slices.Clone(base), SemanticNode{ID: "stop", Parent: "doc", Role: RoleButton, Name: "Stop", Action: "Stop"}), "name", []uiaEvent{
			{kind: uiaChildrenInvalidated, node: "doc"},
		}},
		{"role", edit(func(n []SemanticNode) []SemanticNode { n[5].Role = RoleLink; return n }), "name", []uiaEvent{
			{kind: uiaRetire, node: "send"},
			{kind: uiaChildrenInvalidated, node: "doc"},
		}},
	}
	for _, c := range cases {
		next := uiaTestSnapshot(t, 2, c.focus, c.next...)
		if got, want := uiaEventTexts(uiaChanges(previous, next)), uiaEventTexts(c.want); !slices.Equal(got, want) {
			t.Errorf("%s: events\n%q\nwant\n%q", c.name, got, want)
		}
	}
}

func TestUIAFirstPublishInvalidatesTheWindowAndAnnouncesFocus(t *testing.T) {
	next := uiaTestSnapshot(t, 1, "send",
		SemanticNode{ID: "doc", Role: RoleDocument},
		SemanticNode{ID: "send", Parent: "doc", Role: RoleButton, Name: "Send", Action: "Send"},
	)
	want := []uiaEvent{{kind: uiaChildrenInvalidated}, {kind: uiaRaise, node: "send", id: 20005}}
	if got := uiaEventTexts(uiaChanges(nil, next)); !slices.Equal(got, uiaEventTexts(want)) {
		t.Fatalf("first publish events %q, want %q", got, uiaEventTexts(want))
	}
	retired := uiaChanges(next, nil)
	if got := uiaEventTexts(retired); !slices.Equal(got, uiaEventTexts([]uiaEvent{{kind: uiaRetire, node: "doc"}, {kind: uiaRetire, node: "send"}, {kind: uiaChildrenInvalidated}})) {
		t.Fatalf("clearing the tree produced %q", got)
	}
}

func TestUIANotificationsMapAnnouncementKinds(t *testing.T) {
	cases := []struct {
		kind       AnnouncementKind
		notice     int32
		processing int32
	}{
		{AnnounceProgress, 4, 3},
		{AnnounceDecision, 4, 0},
		{AnnounceCompletion, 2, 0},
		{AnnounceFailure, 3, 0},
	}
	activities := map[string]AnnouncementKind{}
	for _, c := range cases {
		got := uiaNotification(c.kind)
		if got.kind != c.notice || got.processing != c.processing {
			t.Errorf("announcement %d maps to NotificationKind %d processing %d, want %d and %d", c.kind, got.kind, got.processing, c.notice, c.processing)
		}
		if other, dup := activities[got.activity]; dup || got.activity == "" {
			t.Errorf("announcements %d and %d share activity id %q", c.kind, other, got.activity)
		}
		activities[got.activity] = c.kind
	}
}
