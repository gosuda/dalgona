package snapshot

import "errors"

const ConsumerModel = "model"

var ErrNoBoundary = errors.New("snapshot: no delivery boundary")

type Evidence struct {
	Consumer string
	Cutoff   uint64
}

func (e Evidence) Bounded() bool {
	return e.Consumer != "" && e.Cutoff != 0
}

func (s *Store) Bind(ref, consumer string, seq uint64) {
	if s == nil || consumer == "" || seq == 0 {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	entry, ok := s.entries[ref]
	if !ok || entry.Consumer != "" {
		return
	}
	entry.Consumer = consumer
	entry.Delivery = seq
	s.entries[ref] = entry
}

func (s *Store) Eligible(ref string, evidence Evidence) error {
	_, err := s.eligible(ref, evidence)
	return err
}

func (s *Store) Observed(ref string, evidence Evidence, first, last int) (bool, error) {
	entry, err := s.eligible(ref, evidence)
	if err != nil {
		return false, err
	}
	return first >= 1 && last >= first && rangesCover(entry.Shown, first, last), nil
}

func (s *Store) eligible(ref string, evidence Evidence) (Entry, error) {
	if !evidence.Bounded() {
		return Entry{}, ErrNoBoundary
	}
	if s == nil {
		return Entry{}, ErrUnknownReference
	}
	s.mu.RLock()
	defer s.mu.RUnlock()
	entry, ok := s.entries[ref]
	switch {
	case !ok:
		return Entry{}, s.absent(ref)
	case entry.Consumer != "" && entry.Consumer != evidence.Consumer:
		return Entry{}, ErrBindingMismatch
	case entry.Delivery == 0 || entry.Delivery > evidence.Cutoff:
		return Entry{}, ErrUnknownReference
	}
	return entry, nil
}
