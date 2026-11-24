//go:build linux

package platform

import (
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"

	"github.com/godbus/dbus/v5"
	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	atspiRelationErrorMsg uint32 = 21
	atspiRelationErrorFor uint32 = 22
)

type atspiKind uint8

const (
	atspiNodeKind atspiKind = iota
	atspiApplicationKind
	atspiFrameKind
	atspiCacheKind
)

type atspiRoleInfo struct {
	value      uint32
	name       string
	attributes map[string]string
}

var (
	atspiApplicationRole = atspiRoleInfo{value: 75, name: "application"}
	atspiFrameRole       = atspiRoleInfo{value: 23, name: "frame"}
)

var atspiRoles = [...]atspiRoleInfo{
	0:                {0, "invalid", nil},
	RoleButton:       {43, "button", nil},
	RoleCheckBox:     {7, "check box", nil},
	RoleRadioButton:  {44, "radio button", nil},
	RoleSwitch:       {130, "switch", nil},
	RoleTextField:    {79, "entry", nil},
	RoleSecureField:  {40, "password text", nil},
	RoleLink:         {88, "link", nil},
	RoleMenuItem:     {35, "menu item", nil},
	RoleDocument:     {82, "document frame", nil},
	RoleHeading:      {83, "heading", nil},
	RoleList:         {31, "list", nil},
	RoleListItem:     {32, "list item", nil},
	RoleTable:        {55, "table", map[string]string{"xml-roles": "table"}},
	RoleRow:          {90, "table row", nil},
	RoleColumnHeader: {10, "column header", nil},
	RoleRowHeader:    {47, "row header", nil},
	RoleCell:         {56, "table cell", nil},
	RoleCode:         {116, "static", map[string]string{"xml-roles": "code"}},
	RoleMath:         {113, "math", nil},
	RoleFigure:       {39, "panel", map[string]string{"xml-roles": "figure"}},
	RoleImage:        {27, "image", nil},
	RoleTerminal:     {60, "terminal", nil},
	RoleDialog:       {16, "dialog", nil},
	RoleMenu:         {33, "menu", nil},
	RoleStatus:       {54, "status bar", map[string]string{"live": "polite", "container-live": "polite", "container-live-role": "status"}},
	RoleError:        {2, "alert", nil},
}

func atspiRoleOf(r Role) atspiRoleInfo {
	if int(r) >= len(atspiRoles) {
		return atspiRoles[0]
	}
	return atspiRoles[r]
}

type atspiState uint8

const (
	atspiStateActive         atspiState = 1
	atspiStateBusy           atspiState = 3
	atspiStateChecked        atspiState = 4
	atspiStateCollapsed      atspiState = 5
	atspiStateEditable       atspiState = 7
	atspiStateEnabled        atspiState = 8
	atspiStateExpandable     atspiState = 9
	atspiStateExpanded       atspiState = 10
	atspiStateFocusable      atspiState = 11
	atspiStateFocused        atspiState = 12
	atspiStateSelectable     atspiState = 22
	atspiStateSelected       atspiState = 23
	atspiStateSensitive      atspiState = 24
	atspiStateShowing        atspiState = 25
	atspiStateVisible        atspiState = 30
	atspiStateInvalidEntry   atspiState = 36
	atspiStateSelectableText atspiState = 38
	atspiStateCheckable      atspiState = 41
)

var atspiStateEvents = []struct {
	state atspiState
	name  string
}{
	{atspiStateBusy, "busy"},
	{atspiStateEnabled, "enabled"},
	{atspiStateSensitive, "sensitive"},
	{atspiStateFocusable, "focusable"},
	{atspiStateCheckable, "checkable"},
	{atspiStateChecked, "checked"},
	{atspiStateSelectable, "selectable"},
	{atspiStateSelected, "selected"},
	{atspiStateExpandable, "expandable"},
	{atspiStateExpanded, "expanded"},
	{atspiStateCollapsed, "collapsed"},
	{atspiStateEditable, "editable"},
	{atspiStateSelectableText, "selectable-text"},
	{atspiStateInvalidEntry, "invalid-entry"},
	{atspiStateVisible, "visible"},
	{atspiStateShowing, "showing"},
}

type atspiStates [2]uint32

func (s *atspiStates) set(state atspiState, on bool) {
	if on {
		s[state/32] |= 1 << (state % 32)
	}
}

func (s atspiStates) has(state atspiState) bool { return s[state/32]&(1<<(state%32)) != 0 }

func (s atspiStates) words() []uint32 { return s[:] }

type atspiNodeContext struct {
	focus      NodeID
	invalid    bool
	selectable bool
}

func atspiNodeStates(n SemanticNode, context atspiNodeContext) atspiStates {
	checkable := n.Role == RoleCheckBox || n.Role == RoleRadioButton || n.Role == RoleSwitch
	var s atspiStates
	s.set(atspiStateVisible, true)
	s.set(atspiStateShowing, !n.Bounds.Empty())
	s.set(atspiStateEnabled, !n.Disabled)
	s.set(atspiStateSensitive, !n.Disabled)
	s.set(atspiStateFocusable, n.Role.control() || n.Focusable)
	s.set(atspiStateFocused, n.ID == context.focus)
	s.set(atspiStateCheckable, checkable)
	s.set(atspiStateChecked, checkable && n.Checked)
	s.set(atspiStateSelectable, context.selectable)
	s.set(atspiStateSelected, context.selectable && n.selected())
	s.set(atspiStateExpandable, n.Expandable)
	s.set(atspiStateExpanded, n.Expandable && n.Expanded)
	s.set(atspiStateCollapsed, n.Expandable && !n.Expanded)
	s.set(atspiStateBusy, n.TextState == TextLoading)
	s.set(atspiStateEditable, n.Role == RoleTextField || n.Role == RoleSecureField)
	s.set(atspiStateSelectableText, atspiHasText(n))
	s.set(atspiStateInvalidEntry, context.invalid)
	return s
}

func atspiFrameStates(snap *SemanticSnapshot) atspiStates {
	var s atspiStates
	s.set(atspiStateVisible, true)
	s.set(atspiStateEnabled, true)
	s.set(atspiStateSensitive, true)
	s.set(atspiStateShowing, !atspiRootNode(snap).Bounds.Empty())
	s.set(atspiStateActive, atspiFocusOf(snap) != "")
	return s
}

type atspiIndex struct {
	snap   *SemanticSnapshot
	errors map[NodeID][]NodeID
}

func newATSPIIndex(snap *SemanticSnapshot) *atspiIndex {
	index := &atspiIndex{snap: snap, errors: make(map[NodeID][]NodeID)}
	atspiWalk(snap, func(n SemanticNode) {
		if n.Role == RoleError {
			index.errors[n.Target] = append(index.errors[n.Target], n.ID)
		}
	})
	return index
}

func (i *atspiIndex) context(n SemanticNode) atspiNodeContext {
	return atspiNodeContext{focus: atspiFocusOf(i.snap), invalid: len(i.errors[n.ID]) > 0, selectable: atspiSelectableIn(i.snap, n)}
}

func atspiWalk(snap *SemanticSnapshot, visit func(SemanticNode)) {
	if snap == nil {
		return
	}
	pending := []NodeID{snap.Root()}
	for len(pending) > 0 {
		id := pending[len(pending)-1]
		pending = pending[:len(pending)-1]
		node, _ := snap.Node(id)
		visit(node)
		children := snap.Children(id)
		for i := len(children) - 1; i >= 0; i-- {
			pending = append(pending, children[i])
		}
	}
}

func atspiRootNode(snap *SemanticSnapshot) SemanticNode {
	if snap == nil {
		return SemanticNode{}
	}
	root, _ := snap.Node(snap.Root())
	return root
}

func atspiFocusOf(snap *SemanticSnapshot) NodeID {
	if snap == nil {
		return ""
	}
	return snap.Focus()
}

func atspiHas(snap *SemanticSnapshot, id NodeID) bool {
	if snap == nil {
		return false
	}
	_, ok := snap.Node(id)
	return ok
}

func (o *atspiObject) object(kind atspiKind, node SemanticNode) *atspiObject {
	if kind == atspiFrameKind {
		node = atspiRootNode(o.snap)
	}
	return &atspiObject{bridge: o.bridge, bus: o.bus, snap: o.snap, node: node, kind: kind}
}

func (o *atspiObject) self() atspiRef {
	switch o.kind {
	case atspiApplicationKind:
		return o.bus.ref(atspiRootPath)
	case atspiFrameKind:
		return o.bus.ref(atspiFramePath)
	case atspiCacheKind:
		return o.bus.ref(atspiCachePath)
	case atspiNodeKind:
	}
	return o.ref(o.node.ID)
}

func (o *atspiObject) states() atspiStates {
	switch o.kind {
	case atspiApplicationKind, atspiCacheKind:
		return atspiStates{}
	case atspiFrameKind:
		return atspiFrameStates(o.snap)
	case atspiNodeKind:
	}
	return atspiNodeStates(o.node, o.bridge.indexOf(o.snap).context(o.node))
}

func (o *atspiObject) childNodes() []NodeID {
	switch {
	case o.snap == nil || o.kind == atspiApplicationKind || o.kind == atspiCacheKind:
		return nil
	case o.kind == atspiFrameKind:
		return []NodeID{o.snap.Root()}
	}
	return o.snap.Children(o.node.ID)
}

func (o *atspiObject) children() []atspiRef {
	if o.kind == atspiApplicationKind {
		return []atspiRef{o.bus.ref(atspiFramePath)}
	}
	return o.refs(o.childNodes())
}

func (o *atspiObject) ref(id NodeID) atspiRef {
	return o.bus.ref(atspiNodePath(id))
}

func (o *atspiObject) refs(ids []NodeID) []atspiRef {
	refs := make([]atspiRef, 0, len(ids))
	for _, id := range ids {
		refs = append(refs, o.ref(id))
	}
	return refs
}

func (o *atspiObject) parent() atspiRef {
	switch o.kind {
	case atspiApplicationKind:
		if parent := o.bus.parent.Load(); parent != nil {
			return *parent
		}
		return atspiRef{Path: atspiNullPath}
	case atspiFrameKind:
		return o.bus.ref(atspiRootPath)
	case atspiCacheKind:
		return atspiRef{Path: atspiNullPath}
	case atspiNodeKind:
	}
	if o.node.Parent == "" {
		return o.bus.ref(atspiFramePath)
	}
	return o.ref(o.node.Parent)
}

func (o *atspiObject) indexInParent() int32 {
	switch o.kind {
	case atspiApplicationKind, atspiCacheKind:
		return -1
	case atspiFrameKind:
		return 0
	case atspiNodeKind:
	}
	if o.node.Parent == "" {
		return 0
	}
	return clampInt32(slices.Index(o.snap.Children(o.node.Parent), o.node.ID))
}

func (o *atspiObject) name() string {
	if o.kind == atspiNodeKind || o.kind == atspiFrameKind && o.node.Name != "" {
		return o.node.Name
	}
	return o.bridge.application
}

func (o *atspiObject) description() string {
	if o.kind != atspiNodeKind {
		return ""
	}
	return o.node.description()
}

func (o *atspiObject) role() atspiRoleInfo {
	switch o.kind {
	case atspiApplicationKind:
		return atspiApplicationRole
	case atspiFrameKind:
		return atspiFrameRole
	case atspiCacheKind:
		return atspiRoles[0]
	case atspiNodeKind:
	}
	return atspiRoleOf(o.node.Role)
}

func (o *atspiObject) attributes() map[string]string {
	attributes := make(map[string]string)
	if o.kind != atspiNodeKind {
		return attributes
	}
	maps.Copy(attributes, o.role().attributes)
	if o.node.Role == RoleHeading {
		attributes["level"] = strconv.Itoa(o.node.Level)
	}
	if o.node.Placeholder != "" {
		attributes["placeholder-text"] = o.node.Placeholder
	}
	return attributes
}

func (o *atspiObject) interfaces() []string {
	switch o.kind {
	case atspiApplicationKind:
		return []string{atspiAccessible, atspiApplication}
	case atspiFrameKind:
		return []string{atspiAccessible, atspiComponent}
	case atspiCacheKind:
		return []string{atspiCache}
	case atspiNodeKind:
	}
	names := []string{atspiAccessible, atspiComponent}
	for _, optional := range atspiNodeInterfaces {
		if optional.applies(o) {
			names = append(names, optional.name)
		}
	}
	return names
}

var atspiNodeInterfaces = []struct {
	name    string
	applies func(o *atspiObject) bool
}{
	{atspiAction, func(o *atspiObject) bool { return o.node.Action != "" }},
	{atspiText, func(o *atspiObject) bool { return atspiHasText(o.node) }},
	{atspiEditableText, func(o *atspiObject) bool { return o.node.Role == RoleTextField }},
	{atspiSelection, func(o *atspiObject) bool { return len(atspiSelectableChildren(o.snap, o.node)) > 0 }},
	{atspiTable, func(o *atspiObject) bool { return o.node.Role == RoleTable }},
	{atspiTableCell, func(o *atspiObject) bool {
		_, ok := o.cellPlace()
		return ok
	}},
}

func (o *atspiObject) request(action AccessibilityAction, value string, selection inputtext.Range) ([]any, error) {
	if o.snap == nil || o.node.ID == "" {
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.UnknownObject", Body: []any{ErrElementUnavailable.Error()}}
	}
	if o.node.Disabled && atspiChangesControl(action) {
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.AccessDenied", Body: []any{fmt.Sprintf("%q is disabled (%s); enable it in the application before acting on it", o.node.Name, o.node.Reason)}}
	}
	intent := AccessibilityIntent{Node: o.node.ID, Revision: o.snap.Revision(), Action: action, Value: value, Selection: selection}
	err := o.bridge.tree.Request(intent)
	switch {
	case err == nil:
		return []any{true}, nil
	case errors.Is(err, ErrAccessibilityBusy):
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.LimitsExceeded", Body: []any{err.Error()}}
	}
	return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.UnknownObject", Body: []any{err.Error()}}
}

func atspiChangesControl(action AccessibilityAction) bool {
	switch action {
	case ActionActivate, ActionFocus, ActionSetValue:
		return true
	case ActionSelectText, ActionScrollIntoView, ActionLoadText:
		return false
	}
	return true
}

func atspiConstant(value any) atspiProperty {
	return func(*atspiObject) (any, error) { return value, nil }
}

func atspiAnswer(value func(o *atspiObject) any) func(o *atspiObject, _ []any) ([]any, error) {
	return func(o *atspiObject, _ []any) ([]any, error) { return []any{value(o)}, nil }
}

func atspiRead(value func(o *atspiObject) any) atspiProperty {
	return func(o *atspiObject) (any, error) { return value(o), nil }
}

var atspiVersionOne = map[string]atspiProperty{"version": atspiConstant(uint32(1))}

var atspiInterfaces = map[string]atspiInterface{
	atspiAccessible:   {members: atspiAccessibleMembers, properties: atspiAccessibleProperties},
	atspiApplication:  {members: atspiApplicationMembers, properties: atspiApplicationProperties},
	atspiComponent:    {members: atspiComponentMembers, properties: atspiVersionOne},
	atspiAction:       {members: atspiActionMembers, properties: atspiActionProperties},
	atspiText:         {members: atspiTextMembers, properties: atspiTextProperties},
	atspiEditableText: {members: atspiEditableTextMembers, properties: atspiVersionOne},
	atspiSelection:    {members: atspiSelectionMembers, properties: atspiSelectionProperties},
	atspiTable:        {members: atspiTableMembers, properties: atspiTableProperties},
	atspiTableCell:    {members: atspiTableCellMembers, properties: atspiTableCellProperties},
	atspiCache:        {members: atspiCacheMembers, properties: map[string]atspiProperty{"version": atspiConstant(uint32(2))}},
}

var atspiAccessibleProperties = map[string]atspiProperty{
	"version":     atspiConstant(uint32(1)),
	"Name":        atspiRead(func(o *atspiObject) any { return o.name() }),
	"Description": atspiRead(func(o *atspiObject) any { return o.description() }),
	"Parent":      atspiRead(func(o *atspiObject) any { return o.parent() }),
	"ChildCount":  atspiRead(func(o *atspiObject) any { return clampInt32(len(o.children())) }),
	"Locale":      atspiRead(func(o *atspiObject) any { return o.bridge.locale }),
	"AccessibleId": atspiRead(func(o *atspiObject) any {
		if o.kind != atspiNodeKind {
			return ""
		}
		return string(o.node.ID)
	}),
	"HelpText": atspiRead(func(o *atspiObject) any { return o.node.Placeholder }),
}

var atspiAccessibleMembers = map[string]atspiMember{
	"GetChildAtIndex":      {in: []any{int32(0)}, out: []any{atspiRef{}}, call: atspiChildAtIndex},
	"GetChildren":          {out: []any{[]atspiRef{}}, call: atspiAnswer(func(o *atspiObject) any { return o.children() })},
	"GetIndexInParent":     {out: []any{int32(0)}, call: atspiAnswer(func(o *atspiObject) any { return o.indexInParent() })},
	"GetRelationSet":       {out: []any{[]atspiRelation{}}, call: atspiAnswer(atspiRelations)},
	"GetRole":              {out: []any{uint32(0)}, call: atspiAnswer(func(o *atspiObject) any { return o.role().value })},
	"GetRoleName":          {out: []any{""}, call: atspiAnswer(func(o *atspiObject) any { return o.role().name })},
	"GetLocalizedRoleName": {out: []any{""}, call: atspiAnswer(func(o *atspiObject) any { return o.role().name })},
	"GetState":             {out: []any{[]uint32{}}, call: atspiAnswer(func(o *atspiObject) any { return o.states().words() })},
	"GetAttributes":        {out: []any{map[string]string{}}, call: atspiAnswer(func(o *atspiObject) any { return o.attributes() })},
	"GetApplication":       {out: []any{atspiRef{}}, call: atspiAnswer(func(o *atspiObject) any { return o.bus.ref(atspiRootPath) })},
	"GetInterfaces":        {out: []any{[]string{}}, call: atspiAnswer(func(o *atspiObject) any { return o.interfaces() })},
}

var atspiApplicationProperties = map[string]atspiProperty{
	"ToolkitName":      atspiConstant("dalgona"),
	"Version":          atspiRead(func(o *atspiObject) any { return o.bridge.version }),
	"ToolkitVersion":   atspiRead(func(o *atspiObject) any { return o.bridge.version }),
	"AtspiVersion":     atspiConstant("2.1"),
	"InterfaceVersion": atspiConstant(uint32(1)),
	"Id":               atspiRead(func(o *atspiObject) any { return o.bus.id.Load() }),
}

var atspiApplicationMembers = map[string]atspiMember{
	"GetLocale":                {in: []any{uint32(0)}, out: []any{""}, call: atspiAnswer(func(o *atspiObject) any { return o.bridge.locale })},
	"GetApplicationBusAddress": {out: []any{""}, call: atspiAnswer(func(*atspiObject) any { return "" })},
}

type atspiRelation struct {
	Type    uint32
	Targets []atspiRef
}

func atspiChildAtIndex(o *atspiObject, args []any) ([]any, error) {
	index, children := args[0].(int32), o.children()
	if index < 0 || int(index) >= len(children) {
		return nil, atspiInvalid("child %d does not exist because the object has %d children; ask GetChildren for the current list", index, len(children))
	}
	return []any{children[index]}, nil
}

func atspiRelations(o *atspiObject) any {
	relations := []atspiRelation{}
	if o.kind != atspiNodeKind {
		return relations
	}
	if o.node.Role == RoleError {
		relations = append(relations, atspiRelation{Type: atspiRelationErrorFor, Targets: []atspiRef{o.ref(o.node.Target)}})
	}
	if messages := o.bridge.indexOf(o.snap).errors[o.node.ID]; len(messages) > 0 {
		relations = append(relations, atspiRelation{Type: atspiRelationErrorMsg, Targets: o.refs(messages)})
	}
	return relations
}
