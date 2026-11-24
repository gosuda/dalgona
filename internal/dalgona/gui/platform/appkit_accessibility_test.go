package platform

import (
	"bytes"
	"encoding/binary"
	"errors"
	"image"
	"io"
	"reflect"
	"slices"
	"strings"
	"testing"
	"unicode/utf16"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type wireNode struct {
	parent, target    uint32
	role              appKitRole
	subrole           appKitSubrole
	abilities, level  uint8
	bounds            [4]int32
	location, length  uint32
	place             [4]uint32
	id                string
	name, help, value string
	placeholder       string
	states            uint32
	lines             []uint32
	boxes             []wireBox
}

type wireBox struct {
	line     uint32
	bounds   [4]int32
	clusters [][3]int32
}

func wireU32s(b []byte, count uint32) ([]uint32, []byte) {
	values := make([]uint32, count)
	for i := range values {
		values[i] = binary.LittleEndian.Uint32(b[4*i:])
	}
	return values, b[4*count:]
}

func wireRect(b []byte) [4]int32 {
	var r [4]int32
	for k := range r {
		r[k] = wireSigned(b[4*k:])
	}
	return r
}

func wireSigned(b []byte) int32 {
	return int32(binary.LittleEndian.Uint32(b)) //nolint:gosec // decode the wire's two's-complement int32
}

func decodeWireBoxes(b []byte, count uint32) ([]wireBox, []byte) {
	var boxes []wireBox
	for range count {
		box := wireBox{line: binary.LittleEndian.Uint32(b), bounds: wireRect(b[4:])}
		clusters := binary.LittleEndian.Uint32(b[20:])
		b = b[24:]
		for range clusters {
			box.clusters = append(box.clusters, [3]int32{wireSigned(b), wireSigned(b[4:]), wireSigned(b[8:])})
			b = b[12:]
		}
		boxes = append(boxes, box)
	}
	return boxes, b
}

func decodeWireSnapshot(t *testing.T, b []byte) (revision uint64, focus uint32, nodes []wireNode) {
	t.Helper()
	revision, count, focus := binary.LittleEndian.Uint64(b), binary.LittleEndian.Uint32(b[8:]), binary.LittleEndian.Uint32(b[12:])
	b = b[16:]
	for range count {
		n := wireNode{
			parent: binary.LittleEndian.Uint32(b), target: binary.LittleEndian.Uint32(b[4:]),
			role: appKitRole(b[8]), subrole: appKitSubrole(b[9]), abilities: b[10], level: b[11],
			bounds: wireRect(b[12:]), location: binary.LittleEndian.Uint32(b[28:]), length: binary.LittleEndian.Uint32(b[32:]),
		}
		idLength := binary.LittleEndian.Uint32(b[36:])
		units := [5]uint32{binary.LittleEndian.Uint32(b[40:]), binary.LittleEndian.Uint32(b[44:]), binary.LittleEndian.Uint32(b[48:]), binary.LittleEndian.Uint32(b[76:]), binary.LittleEndian.Uint32(b[92:])}
		place, _ := wireU32s(b[52:], 4)
		n.place = [4]uint32(place)
		lineCount, boxCount := binary.LittleEndian.Uint32(b[68:]), binary.LittleEndian.Uint32(b[72:])
		n.states = binary.LittleEndian.Uint32(b[80:])
		b = b[96:]
		n.id, b = string(b[:idLength]), b[idLength:]
		texts := [5]string{}
		for k, u := range units {
			code := make([]uint16, u)
			for i := range code {
				code[i] = binary.LittleEndian.Uint16(b[2*i:])
			}
			texts[k], b = string(utf16.Decode(code)), b[2*u:]
		}
		n.name, n.help, n.value, n.placeholder = texts[0], texts[1], texts[2], texts[3]
		n.lines, b = wireU32s(b, lineCount)
		n.boxes, b = decodeWireBoxes(b, boxCount)
		nodes = append(nodes, n)
	}
	if len(b) != 0 {
		t.Fatalf("%d bytes follow the last node", len(b))
	}
	return revision, focus, nodes
}

func publishedTree(t *testing.T, revision uint64, nodes []SemanticNode) (*AccessibilityTree, *SemanticSnapshot) {
	t.Helper()
	snap, err := NewSemanticSnapshot(revision, "composer", nodes)
	if err != nil {
		t.Fatal(err)
	}
	tree := NewAccessibilityTree(4, nil)
	tree.Publish(snap)
	return tree, snap
}

func TestAppKitSnapshotKeepsReadingOrderAndRelations(t *testing.T) {
	nodes := validTree()
	nodes[2].Value, nodes[2].Selection = "a😀b", inputtext.Range{Start: 1, End: 5}
	nodes[2].Bounds = image.Rect(-12, 40, 300, 72)
	_, snap := publishedTree(t, 9, nodes)
	var e appKitSnapshotEncoder
	wire, err := e.encode(snap)
	if err != nil {
		t.Fatal(err)
	}
	revision, focus, got := decodeWireSnapshot(t, wire)
	ids := make([]string, len(got))
	for i, n := range got {
		ids[i] = n.id
	}
	if want := []string{"transcript", "heading", "composer", "token", "token-error", "send"}; !slices.Equal(ids, want) {
		t.Fatalf("wire order %v, want depth-first reading order %v", ids, want)
	}
	relations := [5]uint32{focus, got[0].parent, got[4].parent, got[4].target, got[5].parent}
	if revision != 9 || relations != [5]uint32{2, appKitNone, 3, 3, 0} {
		t.Fatalf("revision %d focus %d relations %+v", revision, focus, got)
	}
	checkWireControls(t, got)
}

func checkWireControls(t *testing.T, got []wireNode) {
	t.Helper()
	composer := got[2]
	if composer.value != "a😀b" || [2]uint32{composer.location, composer.length} != [2]uint32{1, 2} || composer.bounds != [4]int32{-12, 40, 300, 72} {
		t.Fatalf("composer %+v, want the emoji selected as UTF-16 range 1+2 and exact bounds", composer)
	}
	const sendAbilities = appKitDisabled | appKitPressable
	if got[5].help != "Connect to a daemon first" || got[5].abilities&sendAbilities != sendAbilities {
		t.Fatalf("send %+v, want the disabled reason as help and a pressable disabled button", got[5])
	}
	if got[3].subrole != appKitSecureTextField || got[1].role != appKitHeading || got[1].level != 2 {
		t.Fatalf("token %+v heading %+v", got[3], got[1])
	}
}

func encodeWire(t *testing.T, nodes []SemanticNode) []wireNode {
	t.Helper()
	_, snap := publishedTree(t, 1, nodes)
	var e appKitSnapshotEncoder
	wire, err := e.encode(snap)
	if err != nil {
		t.Fatal(err)
	}
	_, _, got := decodeWireSnapshot(t, wire)
	return got
}

func TestAppKitSnapshotSendsLinesInUTF16WithTheirGeometry(t *testing.T) {
	nodes := validTree()
	nodes[2].Value = "a😀b\ncd\n"
	nodes[2].Lines = []TextLine{
		{Range: inputtext.Range{Start: 0, End: 7}, Bounds: image.Rect(0, 0, 40, 10), Clusters: []ClusterSpan{{0, 10}, {10, 30}, {30, 40}, {40, 40}}},
		{Range: inputtext.Range{Start: 10, End: 10}, Bounds: image.Rect(0, 20, 0, 30)},
	}
	got := encodeWire(t, nodes)
	composer, token, send := got[2], got[3], got[5]
	if !slices.Equal(composer.lines, []uint32{0, 5, 8}) {
		t.Errorf("composer line starts %v, want UTF-16 starts 0, 5, 8 including the empty last line", composer.lines)
	}
	want := []wireBox{
		{line: 0, bounds: [4]int32{0, 0, 40, 10}, clusters: [][3]int32{{0, 0, 10}, {1, 10, 30}, {3, 30, 40}, {4, 40, 40}}},
		{line: 2, bounds: [4]int32{0, 20, 0, 30}},
	}
	if !reflect.DeepEqual(composer.boxes, want) {
		t.Errorf("composer boxes %+v, want %+v", composer.boxes, want)
	}
	if !slices.Equal(token.lines, []uint32{0}) || len(token.boxes) != 0 {
		t.Errorf("secure field lines %v boxes %v, want one empty line without geometry", token.lines, token.boxes)
	}
	if len(send.lines) != 0 || len(send.boxes) != 0 {
		t.Errorf("a button sent text lines %v %v", send.lines, send.boxes)
	}
}

func TestAppKitSnapshotPlacesTableCellsWithSpansAndHeaders(t *testing.T) {
	nodes := append(validTree(),
		SemanticNode{ID: "table", Parent: "transcript", Role: RoleTable, Name: "Usage"},
		SemanticNode{ID: "r0", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "corner", Parent: "r0", Role: RoleCell},
		SemanticNode{ID: "h-tokens", Parent: "r0", Role: RoleColumnHeader, Name: "Tokens", ColumnSpan: 2},
		SemanticNode{ID: "r1", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "fast", Parent: "r1", Role: RoleRowHeader, Name: "Fast", RowSpan: 2},
		SemanticNode{ID: "in", Parent: "r1", Role: RoleCell, Name: "10"},
		SemanticNode{ID: "out", Parent: "r1", Role: RoleCell, Name: "20"},
		SemanticNode{ID: "r2", Parent: "table", Role: RoleRow},
		SemanticNode{ID: "total", Parent: "r2", Role: RoleCell, Name: "30", ColumnSpan: 2},
	)
	places := map[string][4]uint32{}
	abilities := map[string]uint8{}
	for _, n := range encodeWire(t, nodes) {
		places[n.id], abilities[n.id] = n.place, n.abilities
	}
	want := map[string][4]uint32{
		"table": {appKitNone, 0, appKitNone, 0}, "r0": {appKitNone, 0, appKitNone, 0}, "composer": {appKitNone, 0, appKitNone, 0},
		"corner": {0, 1, 0, 1}, "h-tokens": {0, 1, 1, 2},
		"fast": {1, 2, 0, 1}, "in": {1, 1, 1, 1}, "out": {1, 1, 2, 1},
		"total": {2, 1, 1, 2},
	}
	for id, w := range want {
		if places[id] != w {
			t.Errorf("%s placed %v, want %v", id, places[id], w)
		}
	}
	if abilities["fast"]&appKitRowHeader == 0 || abilities["h-tokens"]&appKitColumnHeader == 0 || abilities["in"]&(appKitRowHeader|appKitColumnHeader) != 0 {
		t.Errorf("header abilities fast %b tokens %b in %b", abilities["fast"], abilities["h-tokens"], abilities["in"])
	}
}

func TestAppKitSnapshotRefusesOversizeText(t *testing.T) {
	nodes := validTree()
	nodes[2].Value = strings.Repeat("a", transferLimit+1)
	_, snap := publishedTree(t, 1, nodes)
	var e appKitSnapshotEncoder
	if _, err := e.encode(snap); err == nil {
		t.Fatal("a node with more than 32 MiB of text was encoded")
	}
}

func TestAppKitRolesCoverEveryRoleAndAction(t *testing.T) {
	for r := RoleButton; r <= RoleError; r++ {
		traits := appKitRoles[r]
		if traits.role == 0 {
			t.Errorf("role %d has no AppKit role", r)
		}
		if r.control() && traits.abilities&(appKitPressable|appKitEditable) == 0 {
			t.Errorf("control role %d exposes no AppKit operation", r)
		}
	}
	if got := appKitRoles[RoleSwitch]; got.role != appKitCheckBox || got.subrole != appKitSwitch {
		t.Errorf("switch maps to %+v, want a check box with the switch subrole", got)
	}
	if got := appKitNodeTraits(SemanticNode{Role: RoleDocument}, true); got.abilities&appKitText == 0 {
		t.Errorf("a leaf document exposes no text queries: %+v", got)
	}
}

func wireIntent(i appKitIntent) []byte {
	b := binary.LittleEndian.AppendUint64(nil, i.revision)
	b = append(b, byte(i.action), 0, 0, 0)
	b = binary.LittleEndian.AppendUint32(b, i.location)
	b = binary.LittleEndian.AppendUint32(b, i.length)
	b = binary.LittleEndian.AppendUint32(b, appKitIndex(len(i.node)))
	b = binary.LittleEndian.AppendUint32(b, appKitIndex(len(i.value)))
	b = append(b, i.node...)
	for _, u := range i.value {
		b = binary.LittleEndian.AppendUint16(b, u)
	}
	return b
}

func resolveWire(t *testing.T, snap *SemanticSnapshot, record []byte) (AccessibilityIntent, error) {
	t.Helper()
	var header [appKitIntentHeader]byte
	intent, err := readAppKitIntent(bytes.NewReader(record), &header)
	if err != nil {
		t.Fatal(err)
	}
	return intent.resolve(snap)
}

func TestAppKitSelectionConvertsNativeUnits(t *testing.T) {
	nodes := validTree()
	nodes[2].Value = "a😀b"
	_, snap := publishedTree(t, 4, nodes)
	selected, err := resolveWire(t, snap, wireIntent(appKitIntent{revision: 4, action: ActionSelectText, location: 3, length: 1, node: "composer"}))
	if err != nil || selected.Selection != (inputtext.Range{Start: 5, End: 6}) || selected.Node != "composer" {
		t.Fatalf("selection %+v, %v; want the b after the emoji at bytes 5 to 6", selected, err)
	}
	if _, selectionErr := resolveWire(t, snap, wireIntent(appKitIntent{revision: 4, action: ActionSelectText, location: 2, node: "composer"})); !errors.Is(selectionErr, inputtext.InvalidOffset) {
		t.Fatalf("a caret between surrogates: %v", selectionErr)
	}
}

func TestAppKitSetValueDecodesUTF16(t *testing.T) {
	_, snap := publishedTree(t, 4, validTree())
	i := appKitIntent{revision: 4, action: ActionSetValue, node: "composer", value: utf16.Encode([]rune("hé😀"))}
	set, err := resolveWire(t, snap, wireIntent(i))
	if err != nil || set.Value != "hé😀" || set.Selection != (inputtext.Range{Start: 7, End: 7}) {
		t.Fatalf("set value %+v, %v", set, err)
	}
	i.value = []uint16{'a', 0xD800}
	if _, err := resolveWire(t, snap, wireIntent(i)); !errors.Is(err, inputtext.InvalidEncoding) {
		t.Fatalf("an unpaired surrogate: %v", err)
	}
}

func TestAppKitIntentRefusesStaleRevision(t *testing.T) {
	_, snap := publishedTree(t, 4, validTree())
	if _, err := resolveWire(t, snap, wireIntent(appKitIntent{revision: 3, action: ActionActivate, node: "send"})); !errors.Is(err, ErrElementUnavailable) {
		t.Fatalf("a stale revision: %v", err)
	}
}

func TestServeAppKitIntentsRequestsAndRefuses(t *testing.T) {
	nodes := validTree()
	nodes[0].TextContainer, nodes[0].TextSelectable = true, true
	nodes[1].TextGroup, nodes[1].Value = "transcript", "A😀"
	nodes = append(nodes, SemanticNode{ID: "next", Parent: "transcript", Role: RoleDocument, TextGroup: "transcript", TextBefore: "\n\n", Value: "BC"})
	tree, _ := publishedTree(t, 6, nodes)
	stream := slices.Concat(
		wireIntent(appKitIntent{revision: 5, action: ActionActivate, node: "send"}),
		wireIntent(appKitIntent{revision: 6, action: ActionFocus, node: "token"}),
		wireIntent(appKitIntent{revision: 6, action: ActionSelectText, location: 1, length: 5, node: "transcript"}),
	)
	var refused []error
	if err := serveAppKitIntents(bytes.NewReader(stream), tree, func(err error) { refused = append(refused, err) }); err != nil {
		t.Fatal(err)
	}
	if len(refused) != 1 || !errors.Is(refused[0], ErrElementUnavailable) {
		t.Fatalf("refusals %v, want one stale element", refused)
	}
	if got := <-tree.Intents(); got != (AccessibilityIntent{Node: "token", Revision: 6, Action: ActionFocus}) {
		t.Fatalf("queued %+v", got)
	}
	want := AccessibilityIntent{Node: "heading", Revision: 6, Action: ActionSelectText, Selection: inputtext.Range{Start: 1, End: 1}, SelectionEnd: "next"}
	select {
	case got := <-tree.Intents():
		if got != want {
			t.Fatalf("UTF-16 group selection queued %+v, want %+v", got, want)
		}
	default:
		t.Fatal("group selection did not reach the intent queue")
	}
}

func TestAppKitIntentReaderRefusesMalformedRecords(t *testing.T) {
	tree, _ := publishedTree(t, 6, validTree())
	press := appKitIntent{revision: 6, action: ActionActivate, node: "send"}
	truncated := wireIntent(press)
	if err := serveAppKitIntents(bytes.NewReader(truncated[:len(truncated)-1]), tree, func(error) {}); !errors.Is(err, errAppKitIntent) {
		t.Fatalf("a truncated record: %v", err)
	}
	press.action = ActionLoadText + 1
	unknown := wireIntent(press)
	if _, err := readAppKitIntent(bytes.NewReader(unknown), new([appKitIntentHeader]byte)); !errors.Is(err, errAppKitIntent) {
		t.Fatalf("an unknown action: %v", err)
	}
	if _, err := readAppKitIntent(bytes.NewReader(nil), new([appKitIntentHeader]byte)); !errors.Is(err, io.EOF) {
		t.Fatalf("a closed pipe: %v", err)
	}
}

func TestAnnouncementPriorityKeepsDecisionsAndFailuresUrgent(t *testing.T) {
	want := map[AnnouncementKind]appKitPriority{
		AnnounceProgress:   appKitPriorityLow,
		AnnounceCompletion: appKitPriorityMedium,
		AnnounceDecision:   appKitPriorityHigh,
		AnnounceFailure:    appKitPriorityHigh,
	}
	for kind, priority := range want {
		if got := announcementPriority(kind); got != priority {
			t.Errorf("kind %d priority %d, want %d", kind, got, priority)
		}
	}
}
