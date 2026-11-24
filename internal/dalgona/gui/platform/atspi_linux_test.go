//go:build linux

package platform

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"image"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const atspiTestBusConfig = `<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
`

const (
	atspiTestDeadline   = 5 * time.Second
	atspiTestText       = "한글 e\u0301👍🏽\nok"
	atspiTestAccessible = "org.a11y.atspi.Accessible"
	atspiTestRoot       = dbus.ObjectPath("/org/a11y/atspi/accessible/root")
	atspiTestFrame      = dbus.ObjectPath("/org/a11y/atspi/accessible/frame")
	atspiTestCache      = dbus.ObjectPath("/org/a11y/atspi/cache")
	atspiTestInvalid    = "org.freedesktop.DBus.Error.InvalidArgs"
	atspiTestDenied     = "org.freedesktop.DBus.Error.AccessDenied"
	atspiTestUnknown    = "org.freedesktop.DBus.Error.UnknownObject"
)

func startATSPITestBus(t *testing.T) (address string, stop func()) {
	t.Helper()
	daemon, err := exec.LookPath("dbus-daemon")
	if err != nil {
		t.Skip("dbus-daemon is not installed; the AT-SPI tests need a private accessibility bus")
	}
	config := filepath.Join(t.TempDir(), "bus.conf")
	if err = os.WriteFile(config, []byte(atspiTestBusConfig), 0o600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(daemon, "--config-file="+config, "--nofork", "--print-address")
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	stop = sync.OnceFunc(func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})
	t.Cleanup(stop)
	line, err := bufio.NewReader(out).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	return strings.TrimSpace(line), stop
}

func connectATSPITestBus(t *testing.T, address string) *dbus.Conn {
	t.Helper()
	conn, err := dbus.Connect(address)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func atspiTestOwn(t *testing.T, conn *dbus.Conn, name string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
	defer cancel()
	var reply uint32
	err := conn.BusObject().CallWithContext(ctx, "org.freedesktop.DBus.RequestName", 0, name, uint32(dbus.NameFlagDoNotQueue)).Store(&reply)
	if err != nil || dbus.RequestNameReply(reply) != dbus.RequestNameReplyPrimaryOwner {
		t.Fatalf("the test could not own %s: reply %d, %v", name, reply, err)
	}
}

type atspiFakeRegistry struct {
	conn       *dbus.Conn
	mu         sync.Mutex
	embedded   []atspiRef
	unembedded []atspiRef
	idErr      error
}

func (r *atspiFakeRegistry) Embed(plug atspiRef) (atspiRef, *dbus.Error) {
	ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
	defer cancel()
	err := r.conn.Object(plug.Name, plug.Path).CallWithContext(ctx, "org.freedesktop.DBus.Properties.Set", 0, "org.a11y.atspi.Application", "Id", dbus.MakeVariant(int32(7))).Err
	r.mu.Lock()
	r.embedded = append(r.embedded, plug)
	r.idErr = err
	r.mu.Unlock()
	return atspiRef{Name: r.conn.Names()[0], Path: atspiTestRoot}, nil
}

func (r *atspiFakeRegistry) Unembed(plug atspiRef) *dbus.Error {
	r.mu.Lock()
	r.unembedded = append(r.unembedded, plug)
	r.mu.Unlock()
	return nil
}

func (r *atspiFakeRegistry) records() (embedded, unembedded []atspiRef, idErr error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.embedded), slices.Clone(r.unembedded), r.idErr
}

func (r *atspiFakeRegistry) root() atspiRef {
	return atspiRef{Name: r.conn.Names()[0], Path: atspiTestRoot}
}

func startATSPIRegistry(t *testing.T, address string) *atspiFakeRegistry {
	t.Helper()
	registry := &atspiFakeRegistry{conn: connectATSPITestBus(t, address)}
	if err := registry.conn.Export(registry, atspiTestRoot, "org.a11y.atspi.Socket"); err != nil {
		t.Fatal(err)
	}
	atspiTestOwn(t, registry.conn, "org.a11y.atspi.Registry")
	return registry
}

func atspiEventually(t *testing.T, what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(atspiTestDeadline)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("%s did not happen within %s", what, atspiTestDeadline)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

type atspiFixture struct {
	t        *testing.T
	stopBus  func()
	registry *atspiFakeRegistry
	client   *dbus.Conn
	tree     *AccessibilityTree
	detach   func()
	app      string
}

func atspiTestTree() []SemanticNode {
	return []SemanticNode{
		{ID: "transcript", Role: RoleDocument, Name: "Transcript", Bounds: image.Rect(5, 5, 800, 600)},
		{ID: "plan", Parent: "transcript", Role: RoleHeading, Name: "Plan", Level: 2, Bounds: image.Rect(10, 10, 300, 40)},
		{ID: "composer/main", Parent: "transcript", Role: RoleTextField, Name: "Message", Action: "Edit", Value: atspiTestText, Selection: inputtext.Range{Start: 7, End: 18}, Bounds: image.Rect(10, 500, 700, 580)},
		{ID: "token", Parent: "transcript", Role: RoleSecureField, Name: "API token", Action: "Edit"},
		{ID: "token-error", Parent: "token", Role: RoleError, Name: "The token is empty", Target: "token"},
		{ID: "send", Parent: "transcript", Role: RoleButton, Name: "Send", Action: "Send", Disabled: true, Reason: "Connect to a daemon first", Bounds: image.Rect(710, 500, 790, 580)},
		{ID: "wrap", Parent: "transcript", Role: RoleSwitch, Name: "Wrap", Action: "Toggle", Checked: true, Bounds: image.Rect(10, 50, 60, 70)},
	}
}

func atspiSnapshot(t *testing.T, revision uint64, focus NodeID, nodes []SemanticNode) *SemanticSnapshot {
	t.Helper()
	snap, err := NewSemanticSnapshot(revision, focus, nodes)
	if err != nil {
		t.Fatal(err)
	}
	return snap
}

func TestATSPIMissingTextRequestsLoad(t *testing.T) {
	nodes := append(atspiTestTree(),
		SemanticNode{ID: "full", Parent: "transcript", Role: RoleDocument, TextContainer: true, TextSelectable: true},
		SemanticNode{ID: "cached", Parent: "full", Role: RoleDocument, TextGroup: "full", Value: "A😀"},
		SemanticNode{ID: "unloaded", Parent: "full", Role: RoleDocument, TextGroup: "full", TextBefore: "\n\n", TextState: TextUncached},
	)
	f := newATSPIFixture(t, 2, atspiSnapshot(t, 1, "", nodes))
	group := atspiNodePath("full")
	args := []any{int32(1), int32(5)}
	f.wantError(group, "org.a11y.atspi.Text.GetText", "org.freedesktop.DBus.Error.Failed", args...)
	f.wantIntent(AccessibilityIntent{Node: "unloaded", Revision: 1, Action: ActionLoadText})
	nodes[len(nodes)-1].TextState, nodes[len(nodes)-1].Value = TextReady, "Loaded text"
	f.tree.Publish(atspiSnapshot(t, 2, "", nodes))
	f.want(group, "org.a11y.atspi.Text.GetText", args, "😀\n\nL")
	f.want(group, "org.a11y.atspi.Text.GetCharacterAtOffset", []any{int32(4)}, int32('L'))
	f.want(group, "org.a11y.atspi.Text.SetSelection", []any{int32(0), int32(1), int32(5)}, true)
	f.wantIntent(AccessibilityIntent{Node: "cached", Revision: 2, Action: ActionSelectText, Selection: inputtext.Range{Start: 1, End: 1}, SelectionEnd: "unloaded"})
}

func newATSPIFixture(t *testing.T, queue int, initial *SemanticSnapshot) *atspiFixture {
	t.Helper()
	address, stopBus := startATSPITestBus(t)
	t.Setenv("AT_SPI_BUS_ADDRESS", address)
	f := &atspiFixture{t: t, stopBus: stopBus, registry: startATSPIRegistry(t, address), tree: NewAccessibilityTree(queue, nil)}
	if initial != nil {
		f.tree.Publish(initial)
	}
	detach, err := AttachATSPI(f.tree, "Dalgona", func() image.Point { return image.Pt(100, 50) })
	if err != nil {
		t.Fatal(err)
	}
	f.detach = detach
	t.Cleanup(detach)
	f.client = connectATSPITestBus(t, address)
	embedded, _, idErr := f.registry.records()
	if len(embedded) != 1 || embedded[0].Path != atspiTestRoot || idErr != nil {
		t.Fatalf("attach sent Embed %+v and setting Id failed with %v; want one Embed of the application root", embedded, idErr)
	}
	f.app = embedded[0].Name
	return f
}

func (f *atspiFixture) call(path dbus.ObjectPath, method string, args ...any) *dbus.Call {
	f.t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
	defer cancel()
	return f.client.Object(f.app, path).CallWithContext(ctx, method, 0, args...)
}

func (f *atspiFixture) want(path dbus.ObjectPath, method string, args []any, want ...any) {
	f.t.Helper()
	got := make([]any, len(want))
	for i, w := range want {
		got[i] = reflect.New(reflect.TypeOf(w)).Interface()
	}
	if err := f.call(path, method, args...).Store(got...); err != nil {
		f.t.Fatalf("%s%v on %s: %v", method, args, path, err)
	}
	for i, w := range want {
		if value := reflect.ValueOf(got[i]).Elem().Interface(); !reflect.DeepEqual(value, w) {
			f.t.Errorf("%s%v on %s returned %#v, want %#v", method, args, path, value, w)
		}
	}
}

func (f *atspiFixture) wantProperty(path dbus.ObjectPath, iface, name string, want any) {
	f.t.Helper()
	var value dbus.Variant
	if err := f.call(path, "org.freedesktop.DBus.Properties.Get", iface, name).Store(&value); err != nil {
		f.t.Fatalf("%s.%s on %s: %v", iface, name, path, err)
	}
	got := reflect.New(reflect.TypeOf(want))
	if err := value.Store(got.Interface()); err != nil || !reflect.DeepEqual(got.Elem().Interface(), want) {
		f.t.Errorf("%s.%s on %s is %v (%v), want %#v", iface, name, path, value, err, want)
	}
}

func (f *atspiFixture) wantError(path dbus.ObjectPath, method, name string, args ...any) {
	f.t.Helper()
	var dbusErr dbus.Error
	if err := f.call(path, method, args...).Err; !errors.As(err, &dbusErr) || dbusErr.Name != name {
		f.t.Errorf("%s%v on %s answered %v, want the D-Bus error %s", method, args, path, err, name)
	}
}

func (f *atspiFixture) wantIntent(want AccessibilityIntent) {
	f.t.Helper()
	select {
	case got := <-f.tree.Intents():
		if got != want {
			f.t.Errorf("queued %+v, want %+v", got, want)
		}
	default:
		f.t.Errorf("no intent was queued, want %+v", want)
	}
}

func (f *atspiFixture) wantNoIntent() {
	f.t.Helper()
	select {
	case got := <-f.tree.Intents():
		f.t.Errorf("a refused call queued %+v", got)
	default:
	}
}

func (f *atspiFixture) ref(id NodeID) atspiRef {
	return atspiRef{Name: f.app, Path: atspiNodePath(id)}
}

func atspiTestStates(states ...uint) []uint32 {
	words := make([]uint32, 2)
	for _, s := range states {
		words[s/32] |= 1 << (s % 32)
	}
	return words
}

func TestATSPIPathsEscapeNodeIdentitiesInjectively(t *testing.T) {
	ids := []NodeID{"a", "A", "a/b", "a_2fb", "a b", "_", "__", "😀", "transcript.block-7", "0"}
	seen := make(map[dbus.ObjectPath]NodeID)
	for _, id := range ids {
		path := atspiNodePath(id)
		if !path.IsValid() {
			t.Fatalf("node %q maps to %q, which the D-Bus object path grammar rejects", id, path)
		}
		if other, dup := seen[path]; dup {
			t.Fatalf("nodes %q and %q share the object path %q", other, id, path)
		}
		seen[path] = id
		if back, ok := atspiNodeID(path); !ok || back != id {
			t.Fatalf("object path %q decodes to %q, %v; want %q", path, back, ok, id)
		}
	}
	for _, path := range []dbus.ObjectPath{atspiNodePrefix + "_41", atspiNodePrefix + "a_2F", atspiNodePrefix + "a_2", atspiNodePrefix, "/org/a11y/atspi/accessible/other/a"} {
		if id, ok := atspiNodeID(path); ok {
			t.Fatalf("the non-canonical path %q resolved to node %q", path, id)
		}
	}
}

func TestATSPIRegistersAndExposesTheTreeInReadingOrder(t *testing.T) {
	f := newATSPIFixture(t, 8, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	transcript := atspiNodePath("transcript")
	f.wantProperty(atspiTestRoot, "org.a11y.atspi.Application", "Id", int32(7))
	f.wantProperty(atspiTestRoot, atspiTestAccessible, "Parent", f.registry.root())
	f.wantProperty(atspiTestRoot, atspiTestAccessible, "Name", "Dalgona")
	f.wantError(atspiTestRoot, "org.freedesktop.DBus.Properties.Set", "org.freedesktop.DBus.Error.PropertyReadOnly", atspiTestAccessible, "Name", dbus.MakeVariant("Renamed"))
	f.want(atspiTestRoot, "org.a11y.atspi.Accessible.GetRole", nil, uint32(75))
	f.want(atspiTestRoot, "org.a11y.atspi.Accessible.GetChildren", nil, []atspiRef{{Name: f.app, Path: atspiTestFrame}})
	f.want(transcript, "org.a11y.atspi.Accessible.GetChildren", nil, []atspiRef{f.ref("plan"), f.ref("composer/main"), f.ref("token"), f.ref("send"), f.ref("wrap")})
	f.want(transcript, "org.a11y.atspi.Accessible.GetChildAtIndex", []any{int32(1)}, f.ref("composer/main"))
	f.wantError(transcript, "org.a11y.atspi.Accessible.GetChildAtIndex", atspiTestInvalid, int32(5))
	f.want(atspiNodePath("send"), "org.a11y.atspi.Accessible.GetIndexInParent", nil, int32(3))
	f.wantProperty(atspiNodePath("plan"), atspiTestAccessible, "Parent", f.ref("transcript"))
	f.wantProperty(transcript, atspiTestAccessible, "Parent", atspiRef{Name: f.app, Path: atspiTestFrame})
	roles := map[NodeID]uint32{"transcript": 82, "plan": 83, "composer/main": 79, "token": 40, "token-error": 2, "send": 43, "wrap": 130}
	for id, role := range roles {
		f.want(atspiNodePath(id), "org.a11y.atspi.Accessible.GetRole", nil, role)
	}
	for _, n := range atspiTestTree() {
		f.wantProperty(atspiNodePath(n.ID), atspiTestAccessible, "Name", n.Name)
	}
	f.wantProperty(atspiNodePath("send"), atspiTestAccessible, "Description", "Connect to a daemon first")
	f.want(atspiNodePath("plan"), "org.a11y.atspi.Accessible.GetAttributes", nil, map[string]string{"level": "2"})
	f.want(atspiNodePath("composer/main"), "org.a11y.atspi.Accessible.GetInterfaces", nil, []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component", "org.a11y.atspi.Action", "org.a11y.atspi.Text", "org.a11y.atspi.EditableText"})
}

func TestATSPIFrameSitsBetweenTheApplicationAndTheTree(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	const active, enabled, sensitive, showing, visible = 1, 8, 24, 25, 30
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetRole", nil, uint32(23))
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetRoleName", nil, "frame")
	f.wantProperty(atspiTestFrame, atspiTestAccessible, "Name", "Transcript")
	f.wantProperty(atspiTestFrame, atspiTestAccessible, "Parent", atspiRef{Name: f.app, Path: atspiTestRoot})
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetIndexInParent", nil, int32(0))
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetChildren", nil, []atspiRef{f.ref("transcript")})
	f.wantProperty(atspiNodePath("transcript"), atspiTestAccessible, "Parent", atspiRef{Name: f.app, Path: atspiTestFrame})
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetState", nil, atspiTestStates(active, enabled, sensitive, showing, visible))
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetInterfaces", nil, []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component"})
	f.want(atspiTestFrame, "org.a11y.atspi.Component.GetExtents", []any{uint32(1)}, atspiRect{X: 5, Y: 5, Width: 795, Height: 595})
	f.want(atspiNodePath("transcript"), "org.a11y.atspi.Accessible.GetIndexInParent", nil, int32(0))
	f.want(atspiNodePath("transcript"), "org.a11y.atspi.Component.GetExtents", []any{uint32(2)}, atspiRect{X: 0, Y: 0, Width: 795, Height: 595})
	f.tree.Publish(atspiSnapshot(t, 2, "", atspiTestTree()))
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetState", nil, atspiTestStates(enabled, sensitive, showing, visible))
}

func TestATSPIMapsEveryRoleToTheATSPIEnumeration(t *testing.T) {
	want := map[Role]struct {
		value uint32
		name  string
	}{
		RoleButton: {43, "button"}, RoleCheckBox: {7, "check box"}, RoleRadioButton: {44, "radio button"}, RoleSwitch: {130, "switch"},
		RoleTextField: {79, "entry"}, RoleSecureField: {40, "password text"}, RoleLink: {88, "link"}, RoleMenuItem: {35, "menu item"},
		RoleDocument: {82, "document frame"}, RoleHeading: {83, "heading"}, RoleList: {31, "list"}, RoleListItem: {32, "list item"},
		RoleTable: {55, "table"}, RoleRow: {90, "table row"}, RoleColumnHeader: {10, "column header"}, RoleRowHeader: {47, "row header"}, RoleCell: {56, "table cell"},
		RoleCode: {116, "static"}, RoleMath: {113, "math"}, RoleFigure: {39, "panel"}, RoleImage: {27, "image"},
		RoleTerminal: {60, "terminal"}, RoleDialog: {16, "dialog"}, RoleMenu: {33, "menu"}, RoleStatus: {54, "status bar"}, RoleError: {2, "alert"},
	}
	nodes := []SemanticNode{{ID: "root", Role: RoleDocument, Name: "Root"}}
	for r := RoleButton; r <= RoleError; r++ {
		nodes = append(nodes, SemanticNode{ID: NodeID(fmt.Sprint("role-", r)), Parent: "root", Role: r, Name: "Named", Action: "Act", Level: 1, Target: "root"})
	}
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", nodes))
	for _, n := range nodes[1:] {
		f.want(atspiNodePath(n.ID), "org.a11y.atspi.Accessible.GetRole", nil, want[n.Role].value)
		f.want(atspiNodePath(n.ID), "org.a11y.atspi.Accessible.GetRoleName", nil, want[n.Role].name)
	}
	f.want(atspiNodePath(NodeID(fmt.Sprint("role-", RoleCode))), "org.a11y.atspi.Accessible.GetAttributes", nil, map[string]string{"xml-roles": "code"})
	f.want(atspiNodePath(NodeID(fmt.Sprint("role-", RoleStatus))), "org.a11y.atspi.Accessible.GetAttributes", nil, map[string]string{"live": "polite", "container-live": "polite", "container-live-role": "status"})
}

func TestATSPIStatesAndRelationsFollowTheNodes(t *testing.T) {
	f := newATSPIFixture(t, 8, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	const checked, editable, enabled, focusable, focused, sensitive, showing, visible, invalidEntry, selectableText, checkable = 4, 7, 8, 11, 12, 24, 25, 30, 36, 38, 41
	want := map[NodeID][]uint32{
		"send":          atspiTestStates(focusable, showing, visible),
		"composer/main": atspiTestStates(enabled, sensitive, focusable, focused, editable, selectableText, showing, visible),
		"token":         atspiTestStates(enabled, sensitive, focusable, editable, selectableText, invalidEntry, visible),
		"wrap":          atspiTestStates(enabled, sensitive, focusable, checkable, checked, showing, visible),
		"plan":          atspiTestStates(enabled, sensitive, showing, visible),
	}
	for id, states := range want {
		f.want(atspiNodePath(id), "org.a11y.atspi.Accessible.GetState", nil, states)
	}
	f.want(atspiNodePath("token-error"), "org.a11y.atspi.Accessible.GetRelationSet", nil, []atspiRelation{{Type: 22, Targets: []atspiRef{f.ref("token")}}})
	f.want(atspiNodePath("token"), "org.a11y.atspi.Accessible.GetRelationSet", nil, []atspiRelation{{Type: 21, Targets: []atspiRef{f.ref("token-error")}}})
}

func TestATSPITextUsesCharacterOffsets(t *testing.T) {
	f := newATSPIFixture(t, 8, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	composer, token := atspiNodePath("composer/main"), atspiNodePath("token")
	f.wantProperty(composer, "org.a11y.atspi.Text", "CharacterCount", int32(10))
	f.wantProperty(composer, "org.a11y.atspi.Text", "CaretOffset", int32(7))
	f.want(composer, "org.freedesktop.DBus.Properties.GetAll", []any{"org.a11y.atspi.Text"}, map[string]dbus.Variant{"version": dbus.MakeVariant(uint32(1)), "CharacterCount": dbus.MakeVariant(int32(10)), "CaretOffset": dbus.MakeVariant(int32(7))})
	f.want(composer, "org.a11y.atspi.Text.GetText", []any{int32(3), int32(7)}, "e\u0301👍🏽")
	f.want(composer, "org.a11y.atspi.Text.GetText", []any{int32(0), int32(-1)}, atspiTestText)
	segments := []struct {
		offset      int32
		granularity uint32
		text        string
		start, end  int32
	}{
		{4, 0, "e\u0301", 3, 5},
		{6, 0, "👍🏽", 5, 7},
		{0, 0, "한", 0, 1},
		{10, 0, "", 10, 10},
		{2, 3, "한글 e\u0301👍🏽\n", 0, 8},
		{9, 3, "ok", 8, 10},
		{9, 4, "ok", 8, 10},
	}
	for _, s := range segments {
		f.want(composer, "org.a11y.atspi.Text.GetStringAtOffset", []any{s.offset, s.granularity}, s.text, s.start, s.end)
	}
	f.want(composer, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(1), uint32(1)}, "한글 ", int32(0), int32(3))
	f.want(composer, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(5), uint32(1)}, "e\u0301👍🏽\n", int32(3), int32(8))
	f.want(composer, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(9), uint32(2)}, "ok", int32(8), int32(10))
	f.want(composer, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(0), uint32(2)}, "한글 e\u0301👍🏽\n", int32(0), int32(8))
	f.wantError(composer, "org.a11y.atspi.Text.GetStringAtOffset", atspiTestInvalid, int32(1), uint32(5))
	f.want(composer, "org.a11y.atspi.Text.GetCharacterExtents", []any{int32(0), uint32(1)}, int32(-1), int32(-1), int32(-1), int32(-1))
	f.want(composer, "org.a11y.atspi.Text.GetCharacterAtOffset", []any{int32(1)}, int32('글'))
	f.want(composer, "org.a11y.atspi.Text.GetNSelections", nil, int32(1))
	f.want(composer, "org.a11y.atspi.Text.GetSelection", []any{int32(0)}, int32(3), int32(7))
	f.wantProperty(token, "org.a11y.atspi.Text", "CharacterCount", int32(0))
	f.want(token, "org.a11y.atspi.Text.GetText", []any{int32(0), int32(-1)}, "")
}

func TestATSPITextMeasuresLaidOutLines(t *testing.T) {
	reply := geometryNode()
	reply.Parent = "transcript"
	nodes := []SemanticNode{{ID: "transcript", Role: RoleDocument, Name: "Transcript", Bounds: image.Rect(5, 5, 800, 600)}, reply}
	f := newATSPIFixture(t, 1, atspiSnapshot(t, 1, "", nodes))
	path := atspiNodePath("reply")
	extents := []struct {
		offset int32
		coord  uint32
		want   [4]int32
	}{
		{3, 1, [4]int32{40, 100, 20, 20}},
		{3, 0, [4]int32{140, 150, 20, 20}},
		{3, 2, [4]int32{35, 95, 20, 20}},
		{7, 1, [4]int32{90, 100, 0, 20}},
		{8, 1, [4]int32{10, 120, 10, 20}},
		{10, 1, [4]int32{30, 120, 0, 20}},
	}
	for _, e := range extents {
		f.want(path, "org.a11y.atspi.Text.GetCharacterExtents", []any{e.offset, e.coord}, e.want[0], e.want[1], e.want[2], e.want[3])
	}
	f.wantError(path, "org.a11y.atspi.Text.GetCharacterExtents", atspiTestInvalid, int32(11), uint32(1))
	f.wantError(path, "org.a11y.atspi.Text.GetCharacterExtents", atspiTestInvalid, int32(3), uint32(3))
	f.want(path, "org.a11y.atspi.Text.GetRangeExtents", []any{int32(1), int32(9), uint32(1)}, int32(10), int32(100), int32(80), int32(40))
	f.want(path, "org.a11y.atspi.Text.GetRangeExtents", []any{int32(9), int32(1), uint32(0)}, int32(110), int32(150), int32(80), int32(40))
	f.want(path, "org.a11y.atspi.Text.GetRangeExtents", []any{int32(4), int32(4), uint32(1)}, int32(-1), int32(-1), int32(-1), int32(-1))
	points := []struct {
		x, y  int32
		coord uint32
		want  int32
	}{
		{45, 110, 1, 3},
		{145, 160, 0, 3},
		{40, 105, 2, 3},
		{105, 110, 1, 6},
		{15, 135, 1, 8},
		{200, 110, 1, -1},
		{50, 300, 1, -1},
	}
	for _, p := range points {
		f.want(path, "org.a11y.atspi.Text.GetOffsetAtPoint", []any{p.x, p.y, p.coord}, p.want)
	}
	f.want(path, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(2), uint32(3)}, "Hi 😀 yo\n", int32(0), int32(8))
	f.want(path, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(10), uint32(3)}, "ok", int32(8), int32(10))
	f.want(path, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(5), uint32(1)}, "yo\n", int32(5), int32(8))
	f.want(path, "org.a11y.atspi.Text.GetStringAtOffset", []any{int32(3), uint32(1)}, "Hi 😀 ", int32(0), int32(5))
}

func TestATSPIActionsAndEditsBecomeIntents(t *testing.T) {
	f := newATSPIFixture(t, 1, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	composer, wrap := atspiNodePath("composer/main"), atspiNodePath("wrap")
	accepted := []struct {
		path   dbus.ObjectPath
		method string
		args   []any
		want   AccessibilityIntent
	}{
		{wrap, "org.a11y.atspi.Action.DoAction", []any{int32(0)}, AccessibilityIntent{Node: "wrap", Revision: 1, Action: ActionActivate}},
		{atspiNodePath("plan"), "org.a11y.atspi.Component.GrabFocus", nil, AccessibilityIntent{Node: "plan", Revision: 1, Action: ActionFocus}},
		{composer, "org.a11y.atspi.EditableText.SetTextContents", []any{"새 글"}, AccessibilityIntent{Node: "composer/main", Revision: 1, Action: ActionSetValue, Value: "새 글", Selection: inputtext.Range{Start: 7, End: 7}}},
		{composer, "org.a11y.atspi.EditableText.InsertText", []any{int32(3), "→!", int32(3)}, AccessibilityIntent{Node: "composer/main", Revision: 1, Action: ActionSetValue, Value: "한글 →e\u0301👍🏽\nok", Selection: inputtext.Range{Start: 10, End: 10}}},
		{composer, "org.a11y.atspi.EditableText.DeleteText", []any{int32(3), int32(5)}, AccessibilityIntent{Node: "composer/main", Revision: 1, Action: ActionSetValue, Value: "한글 👍🏽\nok", Selection: inputtext.Range{Start: 7, End: 7}}},
		{composer, "org.a11y.atspi.Text.SetSelection", []any{int32(0), int32(0), int32(2)}, AccessibilityIntent{Node: "composer/main", Revision: 1, Action: ActionSelectText, Selection: inputtext.Range{Start: 0, End: 6}}},
		{composer, "org.a11y.atspi.Text.SetCaretOffset", []any{int32(10)}, AccessibilityIntent{Node: "composer/main", Revision: 1, Action: ActionSelectText, Selection: inputtext.Range{Start: 21, End: 21}}},
	}
	for _, c := range accepted {
		f.want(c.path, c.method, c.args, true)
		f.wantIntent(c.want)
	}
	refused := []struct {
		path   dbus.ObjectPath
		method string
		args   []any
		want   string
	}{
		{atspiNodePath("send"), "org.a11y.atspi.Action.DoAction", []any{int32(0)}, atspiTestDenied},
		{wrap, "org.a11y.atspi.Action.DoAction", []any{int32(1)}, atspiTestInvalid},
		{composer, "org.a11y.atspi.EditableText.InsertText", []any{int32(4), "x", int32(1)}, atspiTestInvalid},
		{composer, "org.a11y.atspi.EditableText.InsertText", []any{int32(0), "→", int32(1)}, atspiTestInvalid},
		{composer, "org.a11y.atspi.EditableText.DeleteText", []any{int32(5), int32(6)}, atspiTestInvalid},
		{composer, "org.a11y.atspi.EditableText.DeleteText", []any{int32(0), int32(11)}, atspiTestInvalid},
		{atspiNodePath("token"), "org.a11y.atspi.Text.SetCaretOffset", []any{int32(0)}, atspiTestDenied},
		{composer, "org.a11y.atspi.Action.DoAction", []any{"0"}, atspiTestInvalid},
	}
	for _, r := range refused {
		f.wantError(r.path, r.method, r.want, r.args...)
	}
	f.wantNoIntent()
	f.want(wrap, "org.a11y.atspi.Action.DoAction", []any{int32(0)}, true)
	f.wantError(wrap, "org.a11y.atspi.Action.DoAction", "org.freedesktop.DBus.Error.LimitsExceeded", int32(0))
}

func TestATSPIStalePathsAnswerUnknownObject(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", atspiTestTree()))
	plan := atspiNodePath("plan")
	f.want(plan, "org.a11y.atspi.Accessible.GetRole", nil, uint32(83))
	f.tree.Publish(atspiSnapshot(t, 2, "", slices.DeleteFunc(atspiTestTree(), func(n SemanticNode) bool { return n.ID == "plan" })))
	f.wantError(plan, "org.a11y.atspi.Accessible.GetRole", atspiTestUnknown)
	f.wantError(plan, "org.a11y.atspi.Component.GrabFocus", atspiTestUnknown)
	f.wantError(plan, "org.freedesktop.DBus.Properties.Get", atspiTestUnknown, atspiTestAccessible, "Name")
	f.wantNoIntent()
	f.want(atspiNodePath("wrap"), "org.a11y.atspi.Action.DoAction", []any{int32(0)}, true)
	f.wantIntent(AccessibilityIntent{Node: "wrap", Revision: 2, Action: ActionActivate})
	if err := f.call(atspiNodePrefix+"_70lan", "org.a11y.atspi.Accessible.GetRole").Err; err == nil {
		t.Fatal("a non-canonical path for plan answered")
	}
}

func TestATSPIComponentUsesWindowScreenAndParentCoordinates(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", atspiTestTree()))
	composer, transcript := atspiNodePath("composer/main"), atspiNodePath("transcript")
	extents := map[uint32]atspiRect{
		0: {X: 110, Y: 550, Width: 690, Height: 80},
		1: {X: 10, Y: 500, Width: 690, Height: 80},
		2: {X: 5, Y: 495, Width: 690, Height: 80},
	}
	for coord, want := range extents {
		f.want(composer, "org.a11y.atspi.Component.GetExtents", []any{coord}, want)
	}
	f.want(composer, "org.a11y.atspi.Component.Contains", []any{int32(120), int32(560), uint32(0)}, true)
	f.want(composer, "org.a11y.atspi.Component.Contains", []any{int32(20), int32(510), uint32(0)}, false)
	f.want(transcript, "org.a11y.atspi.Component.GetAccessibleAtPoint", []any{int32(720), int32(510), uint32(1)}, f.ref("send"))
	f.want(transcript, "org.a11y.atspi.Component.GetAccessibleAtPoint", []any{int32(2000), int32(10), uint32(1)}, atspiRef{Path: "/org/a11y/atspi/null"})
	f.wantError(composer, "org.a11y.atspi.Component.GetExtents", atspiTestInvalid, uint32(3))
}

type atspiTestSignal struct {
	path    dbus.ObjectPath
	member  string
	detail  string
	detail1 int32
	detail2 int32
	value   string
}

func (f *atspiFixture) listen() <-chan *dbus.Signal {
	f.t.Helper()
	signals := make(chan *dbus.Signal, 256)
	f.client.Signal(signals)
	ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
	defer cancel()
	if err := f.client.AddMatchSignalContext(ctx, dbus.WithMatchSender(f.app)); err != nil {
		f.t.Fatal(err)
	}
	return signals
}

func (f *atspiFixture) collect(signals <-chan *dbus.Signal, prefix string, count int) []*dbus.Signal {
	f.t.Helper()
	var got []*dbus.Signal
	timeout := time.After(atspiTestDeadline)
	for len(got) < count {
		select {
		case s := <-signals:
			if strings.HasPrefix(s.Name, prefix) && s.Sender == f.app {
				got = append(got, s)
			}
		case <-timeout:
			f.t.Fatalf("received %d %s signals within %s, want %d", len(got), prefix, atspiTestDeadline, count)
		}
	}
	return got
}

func (f *atspiFixture) expectSignals(signals <-chan *dbus.Signal, want []atspiTestSignal) {
	f.t.Helper()
	var got []atspiTestSignal
	for _, s := range f.collect(signals, "org.a11y.atspi.Event.", len(want)) {
		got = append(got, atspiTestRecord(f.t, s))
	}
	if !slices.Equal(got, want) {
		f.t.Fatalf("received\n%+v\nwant\n%+v", got, want)
	}
}

func atspiTestRecord(t *testing.T, s *dbus.Signal) atspiTestSignal {
	t.Helper()
	record := atspiTestSignal{path: s.Path, member: strings.TrimPrefix(s.Name, "org.a11y.atspi.Event.")}
	var value dbus.Variant
	var properties map[string]dbus.Variant
	if err := dbus.Store(s.Body, &record.detail, &record.detail1, &record.detail2, &value, &properties); err != nil {
		t.Fatalf("signal %s carries %v, want the siiva{sv} event body: %v", s.Name, s.Body, err)
	}
	if len(properties) != 0 {
		t.Fatalf("signal %s carries properties %v, want the empty dictionary the event XML reserves", s.Name, properties)
	}
	record.value = fmt.Sprint(value.Value())
	return record
}

func TestATSPIEmitsChangeAndAnnouncementSignals(t *testing.T) {
	f := newATSPIFixture(t, 4, nil)
	signals := f.listen()
	ref := func(id NodeID) string { return fmt.Sprint([]any{f.app, atspiNodePath(id)}) }
	composer, send := atspiNodePath("composer/main"), atspiNodePath("send")
	f.wantError(atspiTestFrame, "org.a11y.atspi.Component.GrabFocus", atspiTestUnknown)
	f.wantProperty(atspiTestFrame, atspiTestAccessible, "Name", "Dalgona")
	f.tree.Publish(atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	f.expectSignals(signals, []atspiTestSignal{
		{path: atspiTestFrame, member: "Object.ChildrenChanged", detail: "add", value: ref("transcript")},
		{path: atspiTestFrame, member: "Object.PropertyChange", detail: "accessible-name", value: "Transcript"},
		{path: atspiTestFrame, member: "Object.StateChanged", detail: "showing", detail1: 1, value: "0"},
		{path: atspiTestFrame, member: "Window.Activate", value: "Transcript"},
		{path: atspiTestFrame, member: "Object.StateChanged", detail: "active", detail1: 1, value: "0"},
		{path: composer, member: "Object.StateChanged", detail: "focused", detail1: 1, value: "0"},
	})
	nodes := atspiTestTree()
	nodes[1].Name = "Plan v2"
	nodes[2].Value, nodes[2].Selection = atspiTestText+"ay", inputtext.Range{Start: 23, End: 23}
	nodes[5].Disabled = false
	nodes[6] = SemanticNode{ID: "retry", Parent: "transcript", Role: RoleButton, Name: "Retry", Action: "Retry"}
	f.tree.Publish(atspiSnapshot(t, 2, "send", nodes))
	f.tree.Announce(Announcement{Kind: AnnounceFailure, Text: "Send failed"})
	f.expectSignals(signals, []atspiTestSignal{
		{path: atspiNodePath("transcript"), member: "Object.ChildrenChanged", detail: "remove", detail1: 4, value: ref("wrap")},
		{path: atspiNodePath("transcript"), member: "Object.ChildrenChanged", detail: "add", detail1: 4, value: ref("retry")},
		{path: atspiNodePath("plan"), member: "Object.PropertyChange", detail: "accessible-name", value: "Plan v2"},
		{path: composer, member: "Object.TextChanged", detail: "insert", detail1: 10, detail2: 2, value: "ay"},
		{path: composer, member: "Object.TextCaretMoved", detail1: 12, value: "0"},
		{path: composer, member: "Object.TextSelectionChanged", value: ""},
		{path: send, member: "Object.PropertyChange", detail: "accessible-description", value: ""},
		{path: send, member: "Object.StateChanged", detail: "enabled", detail1: 1, value: "0"},
		{path: send, member: "Object.StateChanged", detail: "sensitive", detail1: 1, value: "0"},
		{path: composer, member: "Object.StateChanged", detail: "focused", value: "0"},
		{path: send, member: "Object.StateChanged", detail: "focused", detail1: 1, value: "0"},
		{path: send, member: "Object.Announcement", detail1: 2, value: "Send failed"},
	})
	nodes[2].Value, nodes[2].Selection = "한글 okay", inputtext.Range{Start: 11, End: 11}
	f.tree.Publish(atspiSnapshot(t, 3, "send", nodes))
	f.expectSignals(signals, []atspiTestSignal{
		{path: composer, member: "Object.TextChanged", detail: "delete", detail1: 3, detail2: 5, value: "e\u0301👍🏽\n"},
		{path: composer, member: "Object.TextCaretMoved", detail1: 7, value: "0"},
	})
	f.tree.Publish(atspiSnapshot(t, 4, "", nodes))
	f.expectSignals(signals, []atspiTestSignal{
		{path: send, member: "Object.StateChanged", detail: "focused", value: "0"},
		{path: atspiTestFrame, member: "Window.Deactivate", value: "Transcript"},
		{path: atspiTestFrame, member: "Object.StateChanged", detail: "active", value: "0"},
	})
}

type atspiFakeLauncher string

func (l atspiFakeLauncher) GetAddress() (string, *dbus.Error) { return string(l), nil }

func TestATSPIDiscoversTheBusThroughTheSessionLauncher(t *testing.T) {
	address, _ := startATSPITestBus(t)
	t.Setenv("AT_SPI_BUS_ADDRESS", "")
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	tree := NewAccessibilityTree(4, nil)
	if _, err := AttachATSPI(tree, "Dalgona", nil); err == nil {
		t.Fatal("attach succeeded without an accessibility bus launcher")
	}
	launcher := connectATSPITestBus(t, address)
	if err := launcher.Export(atspiFakeLauncher(address), "/org/a11y/bus", "org.a11y.Bus"); err != nil {
		t.Fatal(err)
	}
	atspiTestOwn(t, launcher, "org.a11y.Bus")
	registry := startATSPIRegistry(t, address)
	detach, err := AttachATSPI(tree, "Dalgona", nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(detach)
	if embedded, _, _ := registry.records(); len(embedded) != 1 {
		t.Fatalf("the registry saw %d Embed calls, want 1", len(embedded))
	}
}

func TestATSPIDetachUnregistersAndLeavesTheBus(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", atspiTestTree()))
	f.detach()
	atspiEventually(t, "Unembed", func() bool {
		_, unembedded, _ := f.registry.records()
		return slices.Equal(unembedded, []atspiRef{{Name: f.app, Path: atspiTestRoot}})
	})
	atspiEventually(t, "leaving the bus", func() bool {
		ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
		defer cancel()
		var owned bool
		err := f.client.BusObject().CallWithContext(ctx, "org.freedesktop.DBus.NameHasOwner", 0, f.app).Store(&owned)
		return err == nil && !owned
	})
	f.tree.Publish(atspiSnapshot(t, 2, "", atspiTestTree()))
	f.detach()
}

func TestATSPIRegistersAgainAfterRegistryAndBusRestarts(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", atspiTestTree()))
	if err := f.registry.conn.Emit(atspiTestRoot, "org.a11y.atspi.Socket.Available", f.registry.root()); err != nil {
		t.Fatal(err)
	}
	time.Sleep(250 * time.Millisecond)
	if embedded, _, _ := f.registry.records(); len(embedded) != 1 {
		t.Fatalf("Available from the registry that holds the application caused %d Embed calls, want 1", len(embedded))
	}
	_ = f.registry.conn.Close()
	replacement := startATSPIRegistry(t, os.Getenv("AT_SPI_BUS_ADDRESS"))
	if err := replacement.conn.Emit(atspiTestRoot, "org.a11y.atspi.Socket.Available", replacement.root()); err != nil {
		t.Fatal(err)
	}
	atspiEventually(t, "Embed with the registry that replaced the first", func() bool {
		embedded, _, _ := replacement.records()
		return len(embedded) == 1
	})
	f.stopBus()
	address, _ := startATSPITestBus(t)
	t.Setenv("AT_SPI_BUS_ADDRESS", address)
	registry := startATSPIRegistry(t, address)
	watcher := connectATSPITestBus(t, address)
	ready := make(chan *dbus.Signal, 16)
	watcher.Signal(ready)
	ctx, cancel := context.WithTimeout(context.Background(), atspiTestDeadline)
	defer cancel()
	if err := watcher.AddMatchSignalContext(ctx, dbus.WithMatchInterface("org.a11y.atspi.Cache"), dbus.WithMatchMember("Ready")); err != nil {
		t.Fatal(err)
	}
	atspiEventually(t, "Embed on the restarted bus", func() bool {
		embedded, _, _ := registry.records()
		return len(embedded) == 1
	})
	embedded, _, _ := registry.records()
	f.client, f.app = watcher, embedded[0].Name
	f.collect(ready, "org.a11y.atspi.Cache.Ready", 1)
	f.want(atspiTestFrame, "org.a11y.atspi.Accessible.GetChildren", nil, []atspiRef{f.ref("transcript")})
}

func atspiTestTable() []SemanticNode {
	return []SemanticNode{
		{ID: "doc", Role: RoleDocument, Name: "Usage"},
		{ID: "table", Parent: "doc", Role: RoleTable, Name: "Files"},
		{ID: "head", Parent: "table", Role: RoleRow},
		{ID: "h-name", Parent: "head", Role: RoleColumnHeader, Name: "Name"},
		{ID: "h-size", Parent: "head", Role: RoleColumnHeader, Name: "Size"},
		{ID: "caption", Parent: "table", Role: RoleStatus, Name: "Two files"},
		{ID: "r1", Parent: "table", Role: RoleRow},
		{ID: "a", Parent: "r1", Role: RoleCell, Name: "a.go"},
		{ID: "a-size", Parent: "r1", Role: RoleCell, Name: "12"},
		{ID: "r2", Parent: "table", Role: RoleRow},
		{ID: "b", Parent: "r2", Role: RoleCell, Name: "b.go"},
	}
}

func TestATSPITableExposesRowsCellsAndColumnHeaders(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", atspiTestTable()))
	table, cell := atspiNodePath("table"), atspiNodePath("a-size")
	null := atspiRef{Path: "/org/a11y/atspi/null"}
	f.wantProperty(table, "org.a11y.atspi.Table", "NRows", int32(3))
	f.wantProperty(table, "org.a11y.atspi.Table", "NColumns", int32(2))
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(1), int32(1)}, f.ref("a-size"))
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(2), int32(1)}, null)
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(-1), int32(0)}, null)
	f.want(table, "org.a11y.atspi.Table.GetIndexAt", []any{int32(1), int32(1)}, int32(3))
	f.want(table, "org.a11y.atspi.Table.GetIndexAt", []any{int32(2), int32(1)}, int32(-1))
	f.want(table, "org.a11y.atspi.Table.GetRowAtIndex", []any{int32(4)}, int32(2))
	f.want(table, "org.a11y.atspi.Table.GetColumnAtIndex", []any{int32(3)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetRowAtIndex", []any{int32(5)}, int32(-1))
	f.want(table, "org.a11y.atspi.Table.GetRowExtentAt", []any{int32(1), int32(0)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetColumnExtentAt", []any{int32(2), int32(0)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetColumnExtentAt", []any{int32(2), int32(1)}, int32(0))
	f.want(table, "org.a11y.atspi.Table.GetColumnHeader", []any{int32(1)}, f.ref("h-size"))
	f.want(table, "org.a11y.atspi.Table.GetColumnHeader", []any{int32(2)}, null)
	f.want(table, "org.a11y.atspi.Table.GetRowHeader", []any{int32(1)}, null)
	f.want(table, "org.a11y.atspi.Table.GetRowColumnExtentsAtIndex", []any{int32(3)}, true, int32(1), int32(1), int32(1), int32(1), false)
	f.want(table, "org.a11y.atspi.Table.GetRowColumnExtentsAtIndex", []any{int32(9)}, false, int32(0), int32(0), int32(0), int32(0), false)
	f.wantProperty(cell, "org.a11y.atspi.TableCell", "Position", atspiPoint{Row: 1, Column: 1})
	f.wantProperty(cell, "org.a11y.atspi.TableCell", "RowSpan", int32(1))
	f.wantProperty(cell, "org.a11y.atspi.TableCell", "ColumnSpan", int32(1))
	f.wantProperty(cell, "org.a11y.atspi.TableCell", "Table", f.ref("table"))
	f.want(cell, "org.a11y.atspi.TableCell.GetRowColumnSpan", nil, true, int32(1), int32(1), int32(1), int32(1))
	f.want(cell, "org.a11y.atspi.TableCell.GetColumnHeaderCells", nil, []atspiRef{f.ref("h-size")})
	f.want(atspiNodePath("h-size"), "org.a11y.atspi.TableCell.GetColumnHeaderCells", nil, []atspiRef{})
	f.wantProperty(atspiNodePath("b"), "org.a11y.atspi.TableCell", "Position", atspiPoint{Row: 2, Column: 0})
	f.want(atspiNodePath("r1"), "org.a11y.atspi.Accessible.GetInterfaces", nil, []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component"})
	f.want(table, "org.a11y.atspi.Accessible.GetInterfaces", nil, []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component", "org.a11y.atspi.Table"})
}

func TestATSPITableHonorsSpansAndRowHeaders(t *testing.T) {
	nodes := []SemanticNode{
		{ID: "doc", Role: RoleDocument, Name: "Usage"},
		{ID: "table", Parent: "doc", Role: RoleTable, Name: "Tokens"},
		{ID: "r0", Parent: "table", Role: RoleRow},
		{ID: "corner", Parent: "r0", Role: RoleCell},
		{ID: "h-tokens", Parent: "r0", Role: RoleColumnHeader, Name: "Tokens", ColumnSpan: 2},
		{ID: "r1", Parent: "table", Role: RoleRow},
		{ID: "fast", Parent: "r1", Role: RoleRowHeader, Name: "Fast", RowSpan: 2},
		{ID: "in", Parent: "r1", Role: RoleCell, Name: "10"},
		{ID: "out", Parent: "r1", Role: RoleCell, Name: "20"},
		{ID: "r2", Parent: "table", Role: RoleRow},
		{ID: "total", Parent: "r2", Role: RoleCell, Name: "30", ColumnSpan: 2},
	}
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "", nodes))
	table, total, fast, in := atspiNodePath("table"), atspiNodePath("total"), atspiNodePath("fast"), atspiNodePath("in")
	null := atspiRef{Path: "/org/a11y/atspi/null"}
	f.wantProperty(table, "org.a11y.atspi.Table", "NRows", int32(3))
	f.wantProperty(table, "org.a11y.atspi.Table", "NColumns", int32(3))
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(2), int32(0)}, f.ref("fast"))
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(0), int32(2)}, f.ref("h-tokens"))
	f.want(table, "org.a11y.atspi.Table.GetAccessibleAt", []any{int32(2), int32(2)}, f.ref("total"))
	f.want(table, "org.a11y.atspi.Table.GetIndexAt", []any{int32(2), int32(0)}, int32(2))
	f.want(table, "org.a11y.atspi.Table.GetIndexAt", []any{int32(0), int32(2)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetRowExtentAt", []any{int32(2), int32(0)}, int32(2))
	f.want(table, "org.a11y.atspi.Table.GetColumnExtentAt", []any{int32(0), int32(1)}, int32(2))
	f.want(table, "org.a11y.atspi.Table.GetColumnExtentAt", []any{int32(2), int32(2)}, int32(2))
	f.want(table, "org.a11y.atspi.Table.GetRowAtIndex", []any{int32(2)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetColumnAtIndex", []any{int32(5)}, int32(1))
	f.want(table, "org.a11y.atspi.Table.GetRowHeader", []any{int32(2)}, f.ref("fast"))
	f.want(table, "org.a11y.atspi.Table.GetRowHeader", []any{int32(0)}, null)
	f.want(table, "org.a11y.atspi.Table.GetColumnHeader", []any{int32(2)}, f.ref("h-tokens"))
	f.want(table, "org.a11y.atspi.Table.GetColumnHeader", []any{int32(0)}, null)
	f.want(table, "org.a11y.atspi.Table.GetRowColumnExtentsAtIndex", []any{int32(5)}, true, int32(2), int32(1), int32(1), int32(2), false)
	f.wantProperty(total, "org.a11y.atspi.TableCell", "Position", atspiPoint{Row: 2, Column: 1})
	f.wantProperty(total, "org.a11y.atspi.TableCell", "RowSpan", int32(1))
	f.wantProperty(total, "org.a11y.atspi.TableCell", "ColumnSpan", int32(2))
	f.want(total, "org.a11y.atspi.TableCell.GetRowColumnSpan", nil, true, int32(2), int32(1), int32(1), int32(2))
	f.want(total, "org.a11y.atspi.TableCell.GetColumnHeaderCells", nil, []atspiRef{f.ref("h-tokens")})
	f.want(total, "org.a11y.atspi.TableCell.GetRowHeaderCells", nil, []atspiRef{f.ref("fast")})
	f.want(in, "org.a11y.atspi.TableCell.GetRowHeaderCells", nil, []atspiRef{f.ref("fast")})
	f.wantProperty(fast, "org.a11y.atspi.TableCell", "RowSpan", int32(2))
	f.want(fast, "org.a11y.atspi.TableCell.GetRowHeaderCells", nil, []atspiRef{})
	f.want(fast, "org.a11y.atspi.TableCell.GetColumnHeaderCells", nil, []atspiRef{})
	f.want(fast, "org.a11y.atspi.Accessible.GetRole", nil, uint32(47))
}

func atspiTestChoices() []SemanticNode {
	return []SemanticNode{
		{ID: "dialog", Role: RoleDialog, Name: "Choose"},
		{ID: "models", Parent: "dialog", Role: RoleList, Name: "Models"},
		{ID: "m1", Parent: "models", Role: RoleListItem, Name: "Fast", Selected: true},
		{ID: "m2", Parent: "models", Role: RoleListItem, Name: "Deep"},
		{ID: "m3", Parent: "models", Role: RoleListItem, Name: "Wide", Selected: true},
		{ID: "note", Parent: "models", Role: RoleStatus, Name: "Two chosen"},
		{ID: "mode", Parent: "dialog", Role: RoleList, Name: "Mode"},
		{ID: "ask", Parent: "mode", Role: RoleRadioButton, Name: "Ask", Action: "Choose", Checked: true},
		{ID: "auto", Parent: "mode", Role: RoleRadioButton, Name: "Auto", Action: "Choose"},
		{ID: "menu", Parent: "dialog", Role: RoleMenu, Name: "More"},
		{ID: "copy", Parent: "menu", Role: RoleMenuItem, Name: "Copy", Action: "Copy"},
		{ID: "paste", Parent: "menu", Role: RoleMenuItem, Name: "Paste", Action: "Paste", Disabled: true, Reason: "The clipboard is empty"},
	}
}

func TestATSPISelectionFollowsCheckedChildren(t *testing.T) {
	f := newATSPIFixture(t, 1, atspiSnapshot(t, 1, "", atspiTestChoices()))
	models, mode, menu := atspiNodePath("models"), atspiNodePath("mode"), atspiNodePath("menu")
	const selectable, selected, enabled, sensitive, visible = 22, 23, 8, 24, 30
	f.wantProperty(models, "org.a11y.atspi.Selection", "NSelectedChildren", int32(2))
	f.want(models, "org.a11y.atspi.Selection.GetSelectedChild", []any{int32(1)}, f.ref("m3"))
	f.want(models, "org.a11y.atspi.Selection.GetSelectedChild", []any{int32(2)}, atspiRef{Path: "/org/a11y/atspi/null"})
	f.want(models, "org.a11y.atspi.Selection.IsChildSelected", []any{int32(0)}, true)
	f.want(models, "org.a11y.atspi.Selection.IsChildSelected", []any{int32(1)}, false)
	f.want(models, "org.a11y.atspi.Selection.IsChildSelected", []any{int32(3)}, false)
	f.wantError(models, "org.a11y.atspi.Selection.IsChildSelected", atspiTestInvalid, int32(4))
	f.want(atspiNodePath("m1"), "org.a11y.atspi.Accessible.GetState", nil, atspiTestStates(enabled, sensitive, selectable, selected, visible))
	f.want(atspiNodePath("m2"), "org.a11y.atspi.Accessible.GetState", nil, atspiTestStates(enabled, sensitive, selectable, visible))
	choices := []struct {
		path   dbus.ObjectPath
		method string
		index  int32
		want   NodeID
	}{
		{models, "SelectChild", 1, "m2"},
		{models, "DeselectChild", 0, "m1"},
		{models, "DeselectSelectedChild", 1, "m3"},
		{mode, "SelectChild", 1, "auto"},
	}
	for _, c := range choices {
		f.want(c.path, "org.a11y.atspi.Selection."+c.method, []any{c.index}, true)
		f.wantIntent(AccessibilityIntent{Node: c.want, Revision: 1, Action: ActionActivate})
	}
	unchanged := []struct {
		path   dbus.ObjectPath
		method string
		args   []any
		want   bool
	}{
		{models, "SelectChild", []any{int32(0)}, true},
		{models, "DeselectChild", []any{int32(1)}, true},
		{models, "SelectChild", []any{int32(3)}, false},
		{models, "DeselectSelectedChild", []any{int32(5)}, false},
		{mode, "DeselectChild", []any{int32(0)}, false},
		{models, "SelectAll", nil, false},
		{models, "ClearSelection", nil, false},
	}
	for _, u := range unchanged {
		f.want(u.path, "org.a11y.atspi.Selection."+u.method, u.args, u.want)
	}
	f.wantError(menu, "org.a11y.atspi.Selection.SelectChild", atspiTestDenied, int32(1))
	f.wantNoIntent()
	f.wantProperty(menu, "org.a11y.atspi.Selection", "NSelectedChildren", int32(0))
	f.wantError(atspiNodePath("dialog"), "org.a11y.atspi.Selection.SelectChild", "org.freedesktop.DBus.Error.UnknownInterface", int32(0))
	signals := f.listen()
	nodes := atspiTestChoices()
	nodes[3].Selected = true
	f.tree.Publish(atspiSnapshot(t, 2, "", nodes))
	f.expectSignals(signals, []atspiTestSignal{
		{path: models, member: "Object.SelectionChanged", value: ""},
		{path: atspiNodePath("m2"), member: "Object.StateChanged", detail: "selected", detail1: 1, value: "0"},
	})
}

func TestATSPICacheListsEveryObjectAndTracksChanges(t *testing.T) {
	f := newATSPIFixture(t, 4, atspiSnapshot(t, 1, "composer/main", atspiTestTree()))
	checkATSPICacheItems(t, f)
	checkATSPICacheChanges(t, f)
}

func checkATSPICacheItems(t *testing.T, f *atspiFixture) {
	t.Helper()
	const focusable, sensitive, showing, visible, active, enabled = 11, 24, 25, 30, 1, 8
	f.wantProperty(atspiTestCache, "org.a11y.atspi.Cache", "version", uint32(2))
	var items []atspiCacheItem
	if err := f.call(atspiTestCache, "org.a11y.atspi.Cache.GetItems").Store(&items); err != nil {
		t.Fatal(err)
	}
	app, frame := atspiRef{Name: f.app, Path: atspiTestRoot}, atspiRef{Name: f.app, Path: atspiTestFrame}
	want := map[atspiRef]atspiCacheItem{
		app:           {Object: app, Application: app, Parent: f.registry.root(), Index: -1, ChildCount: 1, Interfaces: []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Application"}, Name: "Dalgona", Role: 75, States: atspiTestStates()},
		frame:         {Object: frame, Application: app, Parent: app, Index: 0, ChildCount: 1, Interfaces: []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component"}, Name: "Transcript", Role: 23, States: atspiTestStates(active, enabled, sensitive, showing, visible)},
		f.ref("send"): {Object: f.ref("send"), Application: app, Parent: f.ref("transcript"), Index: 3, ChildCount: 0, Interfaces: []string{"org.a11y.atspi.Accessible", "org.a11y.atspi.Component", "org.a11y.atspi.Action"}, Name: "Send", Role: 43, Description: "Connect to a daemon first", States: atspiTestStates(focusable, showing, visible)},
	}
	if len(items) != 2+len(atspiTestTree()) {
		t.Fatalf("GetItems listed %d objects, want the application, the frame, and %d nodes", len(items), len(atspiTestTree()))
	}
	for _, item := range items {
		if expected, listed := want[item.Object]; listed && !reflect.DeepEqual(item, expected) {
			t.Errorf("cache item\n%+v\nwant\n%+v", item, expected)
		}
	}
}

func checkATSPICacheChanges(t *testing.T, f *atspiFixture) {
	t.Helper()
	signals := f.listen()
	nodes := atspiTestTree()
	nodes[6] = SemanticNode{ID: "retry", Parent: "transcript", Role: RoleButton, Name: "Retry", Action: "Retry"}
	f.tree.Publish(atspiSnapshot(t, 2, "composer/main", nodes))
	got := f.collect(signals, "org.a11y.atspi.Cache.", 2)
	var added atspiCacheItem
	var removed atspiRef
	if got[0].Name != "org.a11y.atspi.Cache.AddAccessible" || dbus.Store(got[0].Body, &added) != nil {
		t.Fatalf("first cache signal %s %v, want AddAccessible", got[0].Name, got[0].Body)
	}
	if got[1].Name != "org.a11y.atspi.Cache.RemoveAccessible" || dbus.Store(got[1].Body, &removed) != nil {
		t.Fatalf("second cache signal %s %v, want RemoveAccessible", got[1].Name, got[1].Body)
	}
	if [2]atspiRef{added.Object, added.Parent} != [2]atspiRef{f.ref("retry"), f.ref("transcript")} ||
		added.Index != 4 || added.Role != 43 || removed != f.ref("wrap") {
		t.Fatalf("added %+v and removed %+v", added, removed)
	}
}

func TestATSPIMenuStateAndPlaceholderReachTheBus(t *testing.T) {
	nodes := []SemanticNode{
		{ID: "root", Role: RoleDocument},
		{ID: "menu", Parent: "root", Role: RoleButton, Name: "Menu", Action: "activate", Expandable: true, Expanded: true},
		{ID: "edit", Parent: "root", Role: RoleTextField, Name: "Message", Action: "edit", Placeholder: "Write a message"},
	}
	f := newATSPIFixture(t, 1, atspiSnapshot(t, 1, "", nodes))
	const accessible = "org.a11y.atspi.Accessible"
	const enabled, expandable, expanded, collapsed, focusable, sensitive, visible = 8, 9, 10, 5, 11, 24, 30
	menu, edit := atspiNodePath("menu"), atspiNodePath("edit")
	f.want(menu, accessible+".GetState", nil, atspiTestStates(enabled, expandable, expanded, focusable, sensitive, visible))
	f.want(edit, accessible+".GetAttributes", nil, map[string]string{"placeholder-text": "Write a message"})
	f.wantProperty(edit, accessible, "HelpText", "Write a message")
	nodes[1].Expanded = false
	f.tree.Publish(atspiSnapshot(t, 2, "", nodes))
	f.want(menu, accessible+".GetState", nil, atspiTestStates(enabled, expandable, collapsed, focusable, sensitive, visible))
}
