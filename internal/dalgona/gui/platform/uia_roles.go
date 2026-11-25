package platform

import (
	"image"
	"slices"
)

const uiaAppendRuntimeID int32 = 3

const (
	uiaButtonControlType      int32 = 50000
	uiaCheckBoxControlType    int32 = 50002
	uiaEditControlType        int32 = 50004
	uiaHyperlinkControlType   int32 = 50005
	uiaImageControlType       int32 = 50006
	uiaListItemControlType    int32 = 50007
	uiaListControlType        int32 = 50008
	uiaMenuControlType        int32 = 50009
	uiaMenuItemControlType    int32 = 50011
	uiaRadioButtonControlType int32 = 50013
	uiaStatusBarControlType   int32 = 50017
	uiaTextControlType        int32 = 50020
	uiaGroupControlType       int32 = 50026
	uiaDataItemControlType    int32 = 50029
	uiaDocumentControlType    int32 = 50030
	uiaPaneControlType        int32 = 50033
	uiaHeaderItemControlType  int32 = 50035
	uiaTableControlType       int32 = 50036
)

const (
	uiaInvokePattern         int32 = 10000
	uiaValuePattern          int32 = 10002
	uiaExpandCollapsePattern int32 = 10005
	uiaGridPattern           int32 = 10006
	uiaGridItemPattern       int32 = 10007
	uiaSelectionItemPattern  int32 = 10010
	uiaTablePattern          int32 = 10012
	uiaTableItemPattern      int32 = 10013
	uiaTextPattern           int32 = 10014
	uiaTogglePattern         int32 = 10015
	uiaScrollItemPattern     int32 = 10017
)

const (
	uiaControlTypeProperty         int32 = 30003
	uiaNameProperty                int32 = 30005
	uiaHasKeyboardFocusProperty    int32 = 30008
	uiaIsKeyboardFocusableProperty int32 = 30009
	uiaIsEnabledProperty           int32 = 30010
	uiaAutomationIDProperty        int32 = 30011
	uiaHelpTextProperty            int32 = 30013
	uiaIsPasswordProperty          int32 = 30019
	uiaIsOffscreenProperty         int32 = 30022
	uiaValueValueProperty          int32 = 30045
	uiaExpandCollapseStateProperty int32 = 30070
	uiaIsSelectedProperty          int32 = 30079
	uiaToggleStateProperty         int32 = 30086
	uiaIsDataValidForFormProperty  int32 = 30103
	uiaDescribedByProperty         int32 = 30105
	uiaPositionInSetProperty       int32 = 30152
	uiaSizeOfSetProperty           int32 = 30153
	uiaHeadingLevelProperty        int32 = 30173
	uiaIsDialogProperty            int32 = 30174
)

const (
	uiaFocusChangedEvent         int32 = 20005
	uiaElementDeselectedEvent    int32 = 20011
	uiaElementSelectedEvent      int32 = 20012
	uiaTextSelectionChangedEvent int32 = 20014
	uiaTextChangedEvent          int32 = 20015
)

const (
	uiaNavigateParent          int32 = 0
	uiaNavigateNextSibling     int32 = 1
	uiaNavigatePreviousSibling int32 = 2
	uiaNavigateFirstChild      int32 = 3
	uiaNavigateLastChild       int32 = 4
)

const (
	uiaHeadingLevelNone int32 = 80050
	uiaToggleOff        int32 = 0
	uiaToggleOn         int32 = 1
	uiaCollapsed        int32 = 0
	uiaExpanded         int32 = 1
	uiaLeaf             int32 = 3
)

const (
	uiaNotifyActionCompleted int32 = 2
	uiaNotifyActionAborted   int32 = 3
	uiaNotifyOther           int32 = 4
)

const (
	uiaProcessImportantAll int32 = 0
	uiaProcessMostRecent   int32 = 3
)

type uiaPatternSet uint16

const (
	uiaInvokes uiaPatternSet = 1 << iota
	uiaToggles
	uiaSelects
	uiaHoldsValue
	uiaHoldsText
	uiaScrollsIntoView
	uiaGrids
	uiaGridItems
	uiaExpands
)

type uiaRoleMapping struct {
	controlType int32
	patterns    uiaPatternSet
}

var uiaRoles = [...]uiaRoleMapping{
	RoleButton:       {uiaButtonControlType, uiaInvokes},
	RoleCheckBox:     {uiaCheckBoxControlType, uiaToggles},
	RoleRadioButton:  {uiaRadioButtonControlType, uiaSelects},
	RoleSwitch:       {uiaButtonControlType, uiaToggles},
	RoleTextField:    {uiaEditControlType, uiaHoldsValue | uiaHoldsText},
	RoleSecureField:  {uiaEditControlType, uiaHoldsValue},
	RoleLink:         {uiaHyperlinkControlType, uiaInvokes},
	RoleMenuItem:     {uiaMenuItemControlType, uiaInvokes},
	RoleDocument:     {uiaDocumentControlType, uiaHoldsText},
	RoleHeading:      {uiaTextControlType, 0},
	RoleList:         {uiaListControlType, 0},
	RoleListItem:     {uiaListItemControlType, 0},
	RoleTable:        {uiaTableControlType, uiaGrids},
	RoleRow:          {uiaDataItemControlType, 0},
	RoleColumnHeader: {uiaHeaderItemControlType, uiaGridItems},
	RoleRowHeader:    {uiaHeaderItemControlType, uiaGridItems},
	RoleCell:         {uiaDataItemControlType, uiaGridItems},
	RoleCode:         {uiaTextControlType, uiaHoldsText},
	RoleMath:         {uiaTextControlType, uiaHoldsText},
	RoleFigure:       {uiaGroupControlType, 0},
	RoleImage:        {uiaImageControlType, 0},
	RoleTerminal:     {uiaDocumentControlType, uiaHoldsText},
	RoleDialog:       {uiaPaneControlType, 0},
	RoleMenu:         {uiaMenuControlType, 0},
	RoleStatus:       {uiaStatusBarControlType, 0},
	RoleError:        {uiaTextControlType, 0},
}

var uiaPatternIDs = map[int32]uiaPatternSet{
	uiaInvokePattern:         uiaInvokes,
	uiaTogglePattern:         uiaToggles,
	uiaSelectionItemPattern:  uiaSelects,
	uiaExpandCollapsePattern: uiaExpands,
	uiaValuePattern:          uiaHoldsValue,
	uiaTextPattern:           uiaHoldsText,
	uiaScrollItemPattern:     uiaScrollsIntoView,
	uiaGridPattern:           uiaGrids,
	uiaTablePattern:          uiaGrids,
	uiaGridItemPattern:       uiaGridItems,
	uiaTableItemPattern:      uiaGridItems,
}

func uiaRole(r Role) uiaRoleMapping {
	if int(r) >= len(uiaRoles) {
		return uiaRoleMapping{}
	}
	return uiaRoles[r]
}

func uiaControlType(r Role) int32 {
	return uiaRole(r).controlType
}

func uiaHasPattern(n SemanticNode, patterns uiaPatternSet) bool {
	m := uiaRole(n.Role)
	offered := m.patterns | uiaScrollsIntoView
	if n.Role == RoleDocument && n.Value == "" && n.TextState == TextReady {
		offered &^= uiaHoldsText
	}
	if n.TextContainer {
		offered |= uiaHoldsText
	}
	if n.Selectable {
		offered |= uiaSelects
	}
	if n.Expandable {
		offered |= uiaExpands
	}
	return m.controlType != 0 && offered&patterns == patterns
}

func uiaSupportsPattern(n SemanticNode, pattern int32) bool {
	set, ok := uiaPatternIDs[pattern]
	return ok && uiaHasPattern(n, set)
}

type uiaValueKind uint8

const (
	uiaEmpty uiaValueKind = iota
	uiaString
	uiaBool
	uiaInt
	uiaElements
)

type uiaValue struct {
	kind   uiaValueKind
	text   string
	flag   bool
	number int32
	nodes  []NodeID
}

func uiaText(s string) uiaValue { return uiaValue{kind: uiaString, text: s} }

func uiaFlag(b bool) uiaValue { return uiaValue{kind: uiaBool, flag: b} }

func uiaNumber(i int32) uiaValue { return uiaValue{kind: uiaInt, number: i} }

func (v uiaValue) same(w uiaValue) bool {
	return v.kind == w.kind && v.text == w.text && v.flag == w.flag && v.number == w.number && slices.Equal(v.nodes, w.nodes)
}

type uiaPropertyRule func(s *SemanticSnapshot, n SemanticNode) uiaValue

var uiaProperties = map[int32]uiaPropertyRule{
	uiaControlTypeProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue { return uiaNumber(uiaControlType(n.Role)) },
	uiaNameProperty:        func(_ *SemanticSnapshot, n SemanticNode) uiaValue { return uiaText(n.Name) },
	uiaAutomationIDProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue {
		return uiaText(string(n.ID))
	},
	uiaHelpTextProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue { return uiaOptionalText(uiaHelp(n)) },
	uiaIsSelectedProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue {
		if !uiaHasPattern(n, uiaSelects) {
			return uiaValue{}
		}
		return uiaFlag(n.selected())
	},
	uiaExpandCollapseStateProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue {
		if !n.Expandable {
			return uiaValue{}
		}
		return uiaNumber(uiaExpandState(n))
	},
	uiaIsEnabledProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue { return uiaFlag(!n.Disabled) },
	uiaIsKeyboardFocusableProperty: func(s *SemanticSnapshot, n SemanticNode) uiaValue {
		return uiaFlag(uiaFocusable(s, n))
	},
	uiaHasKeyboardFocusProperty: func(s *SemanticSnapshot, n SemanticNode) uiaValue { return uiaFlag(s.Focus() == n.ID) },
	uiaIsPasswordProperty: func(_ *SemanticSnapshot, n SemanticNode) uiaValue {
		return uiaFlag(n.Role == RoleSecureField)
	},
	uiaIsOffscreenProperty:        func(_ *SemanticSnapshot, n SemanticNode) uiaValue { return uiaFlag(n.Bounds.Empty()) },
	uiaHeadingLevelProperty:       uiaHeadingLevel,
	uiaIsDialogProperty:           uiaDialog,
	uiaDescribedByProperty:        uiaDescribedBy,
	uiaIsDataValidForFormProperty: uiaDataValid,
	uiaPositionInSetProperty: func(s *SemanticSnapshot, n SemanticNode) uiaValue {
		place, _, ok := uiaSetPlace(s, n)
		return uiaSetMember(place, ok)
	},
	uiaSizeOfSetProperty: func(s *SemanticSnapshot, n SemanticNode) uiaValue {
		_, size, ok := uiaSetPlace(s, n)
		return uiaSetMember(size, ok)
	},
}

func uiaProperty(s *SemanticSnapshot, n SemanticNode, property int32) uiaValue {
	rule, ok := uiaProperties[property]
	if !ok {
		return uiaValue{}
	}
	return rule(s, n)
}

func uiaOptionalText(s string) uiaValue {
	if s == "" {
		return uiaValue{}
	}
	return uiaText(s)
}

func uiaHelp(n SemanticNode) string {
	help := n.description()
	if n.Placeholder == "" || help == "" {
		return help + n.Placeholder
	}
	return help + "; " + n.Placeholder
}

func uiaExpandState(n SemanticNode) int32 {
	switch {
	case !n.Expandable:
		return uiaLeaf
	case n.Expanded:
		return uiaExpanded
	default:
		return uiaCollapsed
	}
}

func uiaFocusable(s *SemanticSnapshot, n SemanticNode) bool {
	if s.Focus() == n.ID {
		return true
	}
	return !n.Disabled && (n.Role.control() || n.Role == RoleTerminal || n.Focusable)
}

func uiaHeadingLevel(_ *SemanticSnapshot, n SemanticNode) uiaValue {
	switch {
	case n.Role != RoleHeading:
		return uiaValue{}
	case n.Level < 1 || n.Level > 9:
		return uiaNumber(uiaHeadingLevelNone)
	}
	return uiaNumber(uiaHeadingLevelNone + int32(n.Level))
}

func uiaDialog(_ *SemanticSnapshot, n SemanticNode) uiaValue {
	if n.Role != RoleDialog {
		return uiaValue{}
	}
	return uiaFlag(true)
}

func uiaDescribedBy(s *SemanticSnapshot, n SemanticNode) uiaValue {
	errs := uiaErrorsFor(s, n.ID)
	if len(errs) == 0 {
		return uiaValue{}
	}
	return uiaValue{kind: uiaElements, nodes: errs}
}

func uiaDataValid(s *SemanticSnapshot, n SemanticNode) uiaValue {
	if len(uiaErrorsFor(s, n.ID)) == 0 {
		return uiaValue{}
	}
	return uiaFlag(false)
}

func uiaErrorsFor(s *SemanticSnapshot, target NodeID) []NodeID {
	var errs []NodeID
	for _, n := range uiaNodes(s) {
		if n.Role == RoleError && n.Target == target {
			errs = append(errs, n.ID)
		}
	}
	return errs
}

func uiaSetPlace(s *SemanticSnapshot, n SemanticNode) (place, size int32, ok bool) {
	if n.Role != RoleListItem && n.Role != RoleMenuItem && n.Role != RoleRadioButton {
		return 0, 0, false
	}
	for _, id := range s.Children(n.Parent) {
		if sibling, _ := s.Node(id); sibling.Role == n.Role {
			size++
		}
		if id == n.ID {
			place = size
		}
	}
	return place, size, true
}

func uiaSetMember(n int32, ok bool) uiaValue {
	if !ok {
		return uiaValue{}
	}
	return uiaNumber(n)
}

func uiaToggleState(n SemanticNode) int32 {
	if n.Checked {
		return uiaToggleOn
	}
	return uiaToggleOff
}

func uiaNodes(s *SemanticSnapshot) []SemanticNode {
	if s == nil {
		return nil
	}
	nodes := make([]SemanticNode, 0, len(s.nodes))
	pending := []NodeID{s.Root()}
	for len(pending) > 0 {
		id := pending[len(pending)-1]
		pending = pending[:len(pending)-1]
		n, _ := s.Node(id)
		nodes = append(nodes, n)
		children := s.Children(id)
		for i := len(children) - 1; i >= 0; i-- {
			pending = append(pending, children[i])
		}
	}
	return nodes
}

func uiaNodeIn(s *SemanticSnapshot, id NodeID) (SemanticNode, bool) {
	if s == nil {
		return SemanticNode{}, false
	}
	return s.Node(id)
}

func uiaNavigation(s *SemanticSnapshot, from NodeID, direction int32) (target NodeID, window bool) {
	if s == nil {
		return "", false
	}
	if from == "" {
		return uiaWindowChild(s, direction), false
	}
	n, ok := s.Node(from)
	if !ok {
		return "", false
	}
	switch direction {
	case uiaNavigateParent:
		return n.Parent, n.Parent == ""
	case uiaNavigateNextSibling:
		return uiaSibling(s, n, 1), false
	case uiaNavigatePreviousSibling:
		return uiaSibling(s, n, -1), false
	case uiaNavigateFirstChild:
		return uiaChildAt(s.Children(from), 0), false
	case uiaNavigateLastChild:
		children := s.Children(from)
		return uiaChildAt(children, len(children)-1), false
	}
	return "", false
}

func uiaWindowChild(s *SemanticSnapshot, direction int32) NodeID {
	if direction == uiaNavigateFirstChild || direction == uiaNavigateLastChild {
		return s.Root()
	}
	return ""
}

func uiaSibling(s *SemanticSnapshot, n SemanticNode, step int) NodeID {
	if n.Parent == "" {
		return ""
	}
	siblings := s.Children(n.Parent)
	return uiaChildAt(siblings, slices.Index(siblings, n.ID)+step)
}

func uiaChildAt(children []NodeID, at int) NodeID {
	if at < 0 || at >= len(children) {
		return ""
	}
	return children[at]
}

func uiaRuntimeID(id NodeID) [3]int32 {
	sum := uint64(14695981039346656037)
	for i := range len(id) {
		sum ^= uint64(id[i])
		sum *= 1099511628211
	}
	return [3]int32{uiaAppendRuntimeID, int32(sum >> 33), int32(sum & 0x7fffffff)}
}

func uiaHitTest(s *SemanticSnapshot, p image.Point) NodeID {
	if s == nil {
		return ""
	}
	return uiaHitBelow(s, s.Root(), p)
}

func uiaHitBelow(s *SemanticSnapshot, id NodeID, p image.Point) NodeID {
	n, _ := s.Node(id)
	if !n.Bounds.Empty() && !p.In(n.Bounds) {
		return ""
	}
	children := s.Children(id)
	for i := len(children) - 1; i >= 0; i-- {
		if hit := uiaHitBelow(s, children[i], p); hit != "" {
			return hit
		}
	}
	if n.Bounds.Empty() {
		return ""
	}
	return id
}

type uiaNotice struct {
	kind       int32
	processing int32
	activity   string
}

func uiaNotification(k AnnouncementKind) uiaNotice {
	switch k {
	case AnnounceProgress:
		return uiaNotice{uiaNotifyOther, uiaProcessMostRecent, "dalgona.progress"}
	case AnnounceDecision:
		return uiaNotice{uiaNotifyOther, uiaProcessImportantAll, "dalgona.decision"}
	case AnnounceCompletion:
		return uiaNotice{uiaNotifyActionCompleted, uiaProcessImportantAll, "dalgona.completion"}
	case AnnounceFailure:
		return uiaNotice{uiaNotifyActionAborted, uiaProcessImportantAll, "dalgona.failure"}
	}
	return uiaNotice{uiaNotifyOther, uiaProcessImportantAll, "dalgona.notice"}
}

type uiaEventKind uint8

const (
	uiaRaise uiaEventKind = iota + 1
	uiaPropertyChange
	uiaChildrenInvalidated
	uiaRetire
)

type uiaEvent struct {
	kind     uiaEventKind
	node     NodeID
	role     Role
	id       int32
	previous uiaValue
	next     uiaValue
}

type uiaTrackedProperty struct {
	id       int32
	patterns uiaPatternSet
	value    func(n SemanticNode) uiaValue
}

var uiaTrackedProperties = []uiaTrackedProperty{
	{uiaNameProperty, 0, func(n SemanticNode) uiaValue { return uiaText(n.Name) }},
	{uiaHelpTextProperty, 0, func(n SemanticNode) uiaValue { return uiaText(uiaHelp(n)) }},
	{uiaIsEnabledProperty, 0, func(n SemanticNode) uiaValue { return uiaFlag(!n.Disabled) }},
	{uiaValueValueProperty, uiaHoldsValue, func(n SemanticNode) uiaValue { return uiaText(uiaValueText(n)) }},
	{uiaToggleStateProperty, uiaToggles, func(n SemanticNode) uiaValue { return uiaNumber(uiaToggleState(n)) }},
	{uiaIsSelectedProperty, uiaSelects, func(n SemanticNode) uiaValue { return uiaFlag(n.selected()) }},
	{uiaExpandCollapseStateProperty, uiaExpands, func(n SemanticNode) uiaValue { return uiaNumber(uiaExpandState(n)) }},
}

func uiaValueText(n SemanticNode) string {
	if n.Role == RoleSecureField {
		return ""
	}
	return n.Value
}

func uiaChanges(previous, next *SemanticSnapshot) []uiaEvent {
	events := uiaRetirements(previous, next)
	if uiaRootOf(previous) != uiaRootOf(next) {
		events = append(events, uiaEvent{kind: uiaChildrenInvalidated})
	}
	for _, n := range uiaNodes(next) {
		before, ok := uiaNodeIn(previous, n.ID)
		if !ok || before.Role != n.Role {
			continue
		}
		events = uiaNodeChanges(events, previous, next, before, n)
	}
	return uiaFocusChange(events, previous, next)
}

func uiaRetirements(previous, next *SemanticSnapshot) []uiaEvent {
	var events []uiaEvent
	for _, n := range uiaNodes(previous) {
		now, ok := uiaNodeIn(next, n.ID)
		if ok && now.Role == n.Role {
			continue
		}
		events = append(events, uiaEvent{kind: uiaRetire, node: n.ID, role: n.Role})
		if ok {
			parent, _ := next.Node(now.Parent)
			events = append(events, uiaEvent{kind: uiaChildrenInvalidated, node: parent.ID, role: parent.Role})
		}
	}
	return events
}

func uiaRootOf(s *SemanticSnapshot) NodeID {
	if s == nil {
		return ""
	}
	return s.Root()
}

type uiaPatternEvent struct {
	id       int32
	patterns uiaPatternSet
	fires    func(before, after SemanticNode) bool
}

var uiaPatternEvents = []uiaPatternEvent{
	{uiaTextChangedEvent, uiaHoldsText, func(before, after SemanticNode) bool { return before.Value != after.Value }},
	{uiaTextSelectionChangedEvent, uiaHoldsText, func(before, after SemanticNode) bool { return before.Selection != after.Selection }},
	{uiaElementSelectedEvent, uiaSelects, func(before, after SemanticNode) bool { return !before.selected() && after.selected() }},
	{uiaElementDeselectedEvent, uiaSelects, func(before, after SemanticNode) bool { return before.selected() && !after.selected() }},
}

func uiaNodeChanges(events []uiaEvent, previous, next *SemanticSnapshot, before, after SemanticNode) []uiaEvent {
	if !slices.Equal(previous.Children(after.ID), next.Children(after.ID)) {
		events = append(events, uiaEvent{kind: uiaChildrenInvalidated, node: after.ID, role: after.Role})
	}
	for _, p := range uiaTrackedProperties {
		if !uiaHasPattern(after, p.patterns) {
			continue
		}
		if was, is := p.value(before), p.value(after); !was.same(is) {
			events = append(events, uiaEvent{kind: uiaPropertyChange, node: after.ID, role: after.Role, id: p.id, previous: was, next: is})
		}
	}
	for _, e := range uiaPatternEvents {
		if uiaHasPattern(after, e.patterns) && e.fires(before, after) {
			events = append(events, uiaEvent{kind: uiaRaise, node: after.ID, role: after.Role, id: e.id})
		}
	}
	if after.TextContainer {
		events = uiaTextGroupChanges(events, previous, next, after)
	}
	return events
}

func uiaTextGroupChanges(events []uiaEvent, previous, next *SemanticSnapshot, n SemanticNode) []uiaEvent {
	content, selection := next.textGroupChanges(previous, n.ID)
	if content {
		events = append(events, uiaEvent{kind: uiaRaise, node: n.ID, role: n.Role, id: uiaTextChangedEvent})
	}
	if selection {
		events = append(events, uiaEvent{kind: uiaRaise, node: n.ID, role: n.Role, id: uiaTextSelectionChangedEvent})
	}
	return events
}

func uiaFocusChange(events []uiaEvent, previous, next *SemanticSnapshot) []uiaEvent {
	if next == nil || next.Focus() == "" {
		return events
	}
	focused, _ := next.Node(next.Focus())
	if before, ok := uiaNodeIn(previous, focused.ID); ok && previous.Focus() == focused.ID && before.Role == focused.Role {
		return events
	}
	return append(events, uiaEvent{kind: uiaRaise, node: focused.ID, role: focused.Role, id: uiaFocusChangedEvent})
}
