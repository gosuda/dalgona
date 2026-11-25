package platform

import (
	"errors"
	"sync"
	"sync/atomic"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

var ErrAccessibilityBusy = errors.New("the application is busy; retry the accessibility action")

type AccessibilityAction uint8

const (
	ActionActivate AccessibilityAction = iota + 1
	ActionFocus
	ActionSetValue
	ActionSelectText
	ActionScrollIntoView
	ActionLoadText
)

type AccessibilityIntent struct {
	Node         NodeID
	Revision     uint64
	Action       AccessibilityAction
	Value        string
	Selection    textinput.Range
	SelectionEnd NodeID
}

type textLoadKey struct {
	node     NodeID
	revision uint64
}

type AccessibilityObserver interface {
	SnapshotChanged(previous, next *SemanticSnapshot)
	Announced(n Announcement)
}

type AccessibilityTree struct {
	snapshot  atomic.Pointer[SemanticSnapshot]
	intents   chan AccessibilityIntent
	wake      func()
	mu        sync.Mutex
	observers map[*AccessibilityObserver]AccessibilityObserver
	loads     map[textLoadKey]struct{}
}

func NewAccessibilityTree(queue int, wake func()) *AccessibilityTree {
	return &AccessibilityTree{
		intents:   make(chan AccessibilityIntent, queue),
		wake:      wake,
		observers: make(map[*AccessibilityObserver]AccessibilityObserver),
	}
}

func (t *AccessibilityTree) Snapshot() *SemanticSnapshot { return t.snapshot.Load() }

func (t *AccessibilityTree) Intents() <-chan AccessibilityIntent { return t.intents }

func (t *AccessibilityTree) QueryText(id NodeID, revision uint64) (string, error) {
	s := t.Snapshot()
	if s == nil {
		return "", ErrElementUnavailable
	}
	n, err := s.Lookup(id, revision)
	if err != nil {
		return "", err
	}
	switch n.TextState {
	case TextUncached:
		if err := t.Request(AccessibilityIntent{Node: id, Revision: revision, Action: ActionLoadText}); err != nil {
			return "", err
		}
		return "", ErrAccessibilityBusy
	case TextLoading:
		return "", ErrAccessibilityBusy
	case TextFailed:
		return "", ErrElementUnavailable
	case TextReady:
	}
	if n.Role == RoleSecureField {
		return "", nil
	}
	return n.Value, nil
}

func (t *AccessibilityTree) Publish(next *SemanticSnapshot) {
	previous := t.snapshot.Swap(next)
	for _, o := range t.current() {
		o.SnapshotChanged(previous, next)
	}
}

func (t *AccessibilityTree) Announce(n Announcement) {
	for _, o := range t.current() {
		o.Announced(n)
	}
}

func (t *AccessibilityTree) Observe(o AccessibilityObserver) (stop func()) {
	key := &o
	t.mu.Lock()
	t.observers[key] = o
	t.mu.Unlock()
	return func() {
		t.mu.Lock()
		delete(t.observers, key)
		t.mu.Unlock()
	}
}

func (t *AccessibilityTree) Request(i AccessibilityIntent) error {
	s := t.Snapshot()
	if s == nil {
		return ErrElementUnavailable
	}
	n, err := s.Lookup(i.Node, i.Revision)
	if err != nil {
		return err
	}
	if i.SelectionEnd != "" && !s.acceptsRange(n, i) {
		return ErrElementUnavailable
	}
	queued, err := t.enqueue(i)
	if queued && t.wake != nil {
		t.wake()
	}
	return err
}

func (s *SemanticSnapshot) acceptsRange(start SemanticNode, i AccessibilityIntent) bool {
	end, ok := s.nodes[i.SelectionEnd]
	return ok && i.Action == ActionSelectText && start.TextGroup != "" &&
		end.TextGroup == start.TextGroup && s.nodes[start.TextGroup].TextSelectable
}

func (t *AccessibilityTree) enqueue(i AccessibilityIntent) (bool, error) {
	if i.Action != ActionLoadText {
		return t.send(i)
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	key := textLoadKey{node: i.Node, revision: i.Revision}
	if _, pending := t.loads[key]; pending {
		return false, nil
	}
	queued, err := t.send(i)
	if !queued {
		return false, err
	}
	if t.loads == nil {
		t.loads = make(map[textLoadKey]struct{})
	}
	t.loads[key] = struct{}{}
	return true, nil
}

func (t *AccessibilityTree) send(i AccessibilityIntent) (bool, error) {
	select {
	case t.intents <- i:
		return true, nil
	default:
		return false, ErrAccessibilityBusy
	}
}

func (t *AccessibilityTree) Handled(i AccessibilityIntent) {
	if i.Action != ActionLoadText {
		return
	}
	t.mu.Lock()
	delete(t.loads, textLoadKey{node: i.Node, revision: i.Revision})
	t.mu.Unlock()
}

func (t *AccessibilityTree) current() []AccessibilityObserver {
	t.mu.Lock()
	defer t.mu.Unlock()
	observers := make([]AccessibilityObserver, 0, len(t.observers))
	for _, o := range t.observers {
		observers = append(observers, o)
	}
	return observers
}
