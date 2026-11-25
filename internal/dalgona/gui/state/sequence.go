package state

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	mailboxEvents    = 256
	mailboxBytes     = 8 << 20
	envelopeOverhead = 128
)

var (
	errOverlap = fmt.Errorf("%w: an ordinal arrived twice", rpc.ErrStreamBroken)
	errGap     = fmt.Errorf("%w: an ordinal never arrived", rpc.ErrStreamBroken)
	errMailbox = errors.New("the native session mailbox is full")
)

type envelope struct {
	first, last uint64
	thread      string
	method      string
	params      jsontext.Value
	content     *rpc.ContentReference
	request     *arrival
	at          time.Time
}

type arrival struct {
	id      string
	method  string
	details rpc.ContentSummary
}

func (e envelope) size() int { return len(e.params) + envelopeOverhead }

type sequencer struct{ next uint64 }

func newSequencer() sequencer { return sequencer{next: 1} }

func (s *sequencer) accept(e envelope) error {
	switch {
	case e.first < s.next:
		return errOverlap
	case e.first > s.next:
		return errGap
	}
	s.next = e.last + 1
	return nil
}
