package document

import (
	"errors"
	"fmt"
)

var (
	ErrIdentity   = errors.New("document identity is incomplete")
	ErrClosed     = errors.New("document stream is closed")
	ErrEncoding   = errors.New("document source is not valid UTF-8")
	ErrStale      = errors.New("document revision is stale")
	ErrForeign    = errors.New("position belongs to another document")
	ErrNoPosition = errors.New("document has no selectable text")
)

type Identity struct {
	Endpoint string
	Thread   string
	Turn     string
	Item     string
	Draft    string
}

func Committed(endpoint, thread, turn, item string) (Identity, error) {
	id := Identity{Endpoint: endpoint, Thread: thread, Turn: turn, Item: item}
	if endpoint == "" || thread == "" || turn == "" || item == "" {
		return Identity{}, fmt.Errorf("%w: a committed document needs endpoint, thread, turn, and item identifiers, got %+v", ErrIdentity, id)
	}
	return id, nil
}

func LocalDraft(endpoint, thread, draft string) (Identity, error) {
	if endpoint == "" || draft == "" {
		return Identity{}, fmt.Errorf("%w: a draft document needs an endpoint and a local draft identifier", ErrIdentity)
	}
	return Identity{Endpoint: endpoint, Thread: thread, Draft: draft}, nil
}

func (i Identity) IsDraft() bool {
	return i.Draft != ""
}

func (i Identity) valid() bool {
	if i.IsDraft() {
		return i.Endpoint != "" && i.Turn == "" && i.Item == ""
	}
	return i.Endpoint != "" && i.Thread != "" && i.Turn != "" && i.Item != ""
}

type Revision uint64

type Span struct {
	Start int
	End   int
}

func (s Span) Len() int {
	return s.End - s.Start
}

func (s Span) contains(offset int) bool {
	return s.Start <= offset && offset < s.End
}

type Key struct {
	Document    Identity
	Span        Span
	Kind        Kind
	Hash        uint64
	Provisional bool
}

type Affinity uint8

const (
	Before Affinity = iota + 1
	After
)

type Position struct {
	Document Identity
	Block    Key
	Offset   int
	Affinity Affinity
}

type KeyChange struct {
	From Key
	To   Key
}

type Change struct {
	From     Identity
	To       Identity
	Revision Revision
	Edits    []Edit
	Appended bool
	Keys     []KeyChange
}
