package state

import "github.com/gosuda/dalgona/internal/dal/rpc"

type cutPhase uint8

const (
	cutAbsent cutPhase = iota
	cutOpening
	cutKnown
)

type threadCut struct {
	phase cutPhase
	cut   uint64
	prior bool
	held  []envelope
	bytes int
}

func (c *threadCut) reopen() {
	c.prior = c.phase == cutKnown
	c.phase = cutOpening
	c.held, c.bytes = nil, 0
}

func (c *threadCut) restore() {
	c.phase = cutAbsent
	if c.prior {
		c.phase = cutKnown
	}
	c.prior, c.held, c.bytes = false, nil, 0
}

func (c *threadCut) freeze() { c.prior = false }

func (c *threadCut) route(e envelope) (bool, error) {
	switch c.phase {
	case cutKnown:
		return after(e, c.cut)
	case cutOpening:
		if len(c.held) >= mailboxEvents || c.bytes+e.size() > mailboxBytes {
			return false, errMailbox
		}
		c.held = append(c.held, e)
		c.bytes += e.size()
		if !c.prior {
			return false, nil
		}
		return after(e, c.cut)
	case cutAbsent:
	}
	return false, nil
}

func (c *threadCut) establish(cut uint64) ([]envelope, error) {
	apply := c.held[:0:0]
	for _, e := range c.held {
		later, err := after(e, cut)
		if err != nil {
			return nil, err
		}
		if later {
			apply = append(apply, e)
		}
	}
	*c = threadCut{phase: cutKnown, cut: cut}
	return apply, nil
}

func after(e envelope, cut uint64) (bool, error) {
	return rpc.Event{First: e.first, Last: e.last}.AfterCut(cut)
}
