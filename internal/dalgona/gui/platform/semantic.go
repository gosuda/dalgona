package platform

import (
	"errors"
	"fmt"
	"image"
	"slices"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type Role uint8

const (
	RoleButton Role = iota + 1
	RoleCheckBox
	RoleRadioButton
	RoleSwitch
	RoleTextField
	RoleSecureField
	RoleLink
	RoleMenuItem
	RoleDocument
	RoleHeading
	RoleList
	RoleListItem
	RoleTable
	RoleRow
	RoleColumnHeader
	RoleRowHeader
	RoleCell
	RoleCode
	RoleMath
	RoleFigure
	RoleImage
	RoleTerminal
	RoleDialog
	RoleMenu
	RoleStatus
	RoleError
)

func (r Role) control() bool {
	return r >= RoleButton && r <= RoleMenuItem
}

type NodeID string

type TextAvailability uint8

const (
	TextReady TextAvailability = iota
	TextUncached
	TextLoading
	TextFailed
)

type SemanticNode struct {
	ID           NodeID
	Parent       NodeID
	Role         Role
	Name         string
	Description  string
	Placeholder  string
	Value        string
	Action       string
	Target       NodeID
	Reason       string
	Bounds       image.Rectangle
	Selection    inputtext.Range
	Level        int
	RowSpan      int
	ColumnSpan   int
	TextState    TextAvailability
	TextRevision string

	Lines      []TextLine
	Disabled   bool
	Focusable  bool
	Checked    bool
	Selectable bool
	Selected   bool
	Expandable bool
	Expanded   bool

	TextGroup      NodeID
	TextBefore     string
	TextContainer  bool
	TextSelectable bool
}

var ErrElementUnavailable = errors.New("the accessibility element is no longer available; query the current tree")

func CheckSemantics(nodes []SemanticNode) error {
	ids := make(map[NodeID]bool, len(nodes))
	for _, n := range nodes {
		if n.ID == "" || ids[n.ID] {
			return fmt.Errorf("semantic node %q: identity is empty or repeated", n.ID)
		}
		ids[n.ID] = true
	}
	for _, n := range nodes {
		if err := n.check(ids); err != nil {
			return fmt.Errorf("semantic node %q: %w", n.ID, err)
		}
	}
	return nil
}

type semanticRule struct {
	broken  func(n SemanticNode, ids map[NodeID]bool) bool
	problem string
}

var semanticRules = []semanticRule{
	{func(n SemanticNode, _ map[NodeID]bool) bool {
		return n.TextState > TextFailed || n.TextState != TextReady && n.Value != ""
	}, "text availability does not match the value"},
	{func(n SemanticNode, _ map[NodeID]bool) bool { return n.Role < RoleButton || n.Role > RoleError }, "role is unknown"},
	{func(n SemanticNode, _ map[NodeID]bool) bool { return n.Role.control() && n.Name == "" }, "a control has no name"},
	{func(n SemanticNode, _ map[NodeID]bool) bool { return n.Role.control() && n.Action == "" }, "a control has no operation"},
	{func(n SemanticNode, _ map[NodeID]bool) bool { return n.Role == RoleSecureField && n.Value != "" }, "a secure field exposes its raw value"},
	{func(n SemanticNode, ids map[NodeID]bool) bool { return n.Role == RoleError && !ids[n.Target] }, "an error is not associated with its field or action"},
	{func(n SemanticNode, _ map[NodeID]bool) bool {
		return n.Role == RoleHeading && (n.Level < 1 || n.Level > 6)
	}, "a heading level is outside 1 to 6"},
	{func(n SemanticNode, _ map[NodeID]bool) bool {
		return n.RowSpan < 0 || n.ColumnSpan < 0 || n.RowSpan > spanLimit || n.ColumnSpan > spanLimit
	}, "a cell span is outside 0 to 65534"},
	{func(n SemanticNode, _ map[NodeID]bool) bool {
		return !n.Role.cell() && (n.RowSpan != 0 || n.ColumnSpan != 0)
	}, "a span is set on a node that is not a table cell or header"},
}

func (n SemanticNode) check(ids map[NodeID]bool) error {
	for _, rule := range semanticRules {
		if rule.broken(n, ids) {
			return errors.New(rule.problem)
		}
	}
	return checkLines(n.Value, n.Lines)
}

func (n SemanticNode) description() string {
	if !n.Disabled || n.Reason == "" {
		return n.Description
	}
	if n.Description == "" {
		return n.Reason
	}
	return n.Description + "; " + n.Reason
}

func (n SemanticNode) selected() bool {
	return n.Selected || n.Role == RoleRadioButton && n.Checked
}

type SemanticSnapshot struct {
	revision uint64
	root     NodeID
	focus    NodeID
	nodes    map[NodeID]SemanticNode
	children map[NodeID][]NodeID

	textParts map[NodeID][]NodeID
	textOrder map[NodeID]int
}

func NewSemanticSnapshot(revision uint64, focus NodeID, nodes []SemanticNode) (*SemanticSnapshot, error) {
	if err := CheckSemantics(nodes); err != nil {
		return nil, err
	}
	s := &SemanticSnapshot{
		revision: revision,
		focus:    focus,
		nodes:    make(map[NodeID]SemanticNode, len(nodes)),
		children: make(map[NodeID][]NodeID),
	}
	for _, n := range nodes {
		if err := s.place(n); err != nil {
			return nil, err
		}
		if err := s.placeText(n); err != nil {
			return nil, err
		}
	}
	if s.root == "" {
		return nil, errors.New("semantic tree: it has no root node")
	}
	if _, ok := s.nodes[focus]; focus != "" && !ok {
		return nil, fmt.Errorf("semantic tree: focus %q is not a node", focus)
	}
	return s, nil
}

func (s *SemanticSnapshot) place(n SemanticNode) error {
	_, parentPlaced := s.nodes[n.Parent]
	switch {
	case n.Parent == "" && s.root != "":
		return fmt.Errorf("semantic node %q: a tree has one root", n.ID)
	case n.Parent == "":
		s.root = n.ID
	case !parentPlaced:
		return fmt.Errorf("semantic node %q: its parent does not precede it", n.ID)
	default:
		s.children[n.Parent] = append(s.children[n.Parent], n.ID)
	}
	s.nodes[n.ID] = n
	return nil
}

func (s *SemanticSnapshot) Revision() uint64 { return s.revision }

func (s *SemanticSnapshot) Root() NodeID { return s.root }

func (s *SemanticSnapshot) Focus() NodeID { return s.focus }

func (s *SemanticSnapshot) Node(id NodeID) (SemanticNode, bool) {
	n, ok := s.nodes[id]
	return n, ok
}

func (s *SemanticSnapshot) Children(id NodeID) []NodeID {
	return slices.Clip(s.children[id])
}

func (s *SemanticSnapshot) Lookup(id NodeID, revision uint64) (SemanticNode, error) {
	n, ok := s.nodes[id]
	if !ok || revision != s.revision {
		return SemanticNode{}, ErrElementUnavailable
	}
	return n, nil
}

const TextObjectLimit = 64 << 10

func TextObjects(text string) []inputtext.Range {
	objects := make([]inputtext.Range, 0, len(text)/TextObjectLimit+1)
	for start := 0; start < len(text); {
		end := objectEnd(text, start)
		objects = append(objects, inputtext.Range{Start: start, End: end})
		start = end
	}
	return objects
}

func objectEnd(text string, start int) int {
	if len(text)-start <= TextObjectLimit {
		return len(text)
	}
	end := start
	for {
		next, err := inputtext.NextBoundary(text, end)
		if err != nil || next == end || next-start > TextObjectLimit {
			break
		}
		end = next
	}
	if end > start {
		return end
	}
	end = start + TextObjectLimit
	for !utf8.RuneStart(text[end]) {
		end--
	}
	return end
}

type AnnouncementKind uint8

const (
	AnnounceProgress AnnouncementKind = iota + 1
	AnnounceDecision
	AnnounceCompletion
	AnnounceFailure
)

type Announcement struct {
	Kind AnnouncementKind
	Text string
}

const progressInterval = time.Second

type Announcer struct {
	lastProgress time.Time
	pending      Announcement
}

func (a *Announcer) Announce(now time.Time, n Announcement) (Announcement, bool) {
	if n.Kind != AnnounceProgress {
		a.pending = Announcement{}
		return n, true
	}
	a.pending = n
	return a.Flush(now)
}

func (a *Announcer) Flush(now time.Time) (Announcement, bool) {
	if a.pending.Kind == 0 || now.Before(a.Due()) {
		return Announcement{}, false
	}
	n := a.pending
	a.pending, a.lastProgress = Announcement{}, now
	return n, true
}

func (a *Announcer) Due() time.Time {
	if a.lastProgress.IsZero() {
		return time.Time{}
	}
	return a.lastProgress.Add(progressInterval)
}
