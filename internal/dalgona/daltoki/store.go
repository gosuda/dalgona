package daltoki

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"time"

	"github.com/gosuda/dalgona/internal/dal/journal"
)

const (
	recordVersion = 1

	kindDeliveryAccepted = "daltoki.delivery.accepted"
	kindCursorSaved      = "daltoki.cursor.saved"
	kindJobCreated       = "daltoki.job.created"
	kindJobState         = "daltoki.job.state"
	kindJobThread        = "daltoki.job.thread"
	kindJobPush          = "daltoki.job.push"
	kindDraftCreated     = "daltoki.draft.created"
	kindDraftEdited      = "daltoki.draft.edited"
	kindDraftDecided     = "daltoki.draft.decided"
	kindDraftSending     = "daltoki.draft.sending"
	kindDraftReceipt     = "daltoki.draft.receipt"
	kindDraftUncertain   = "daltoki.draft.uncertain"
	kindControlChanged   = "daltoki.control.changed"
	kindBudgetReserved   = "daltoki.budget.reserved"

	deliveryTombstoneDays = 30
)

var errJournalDamaged = errors.New("daltoki: journal is damaged")

var errDeliveryConflict = errors.New("daltoki: delivery id conflict")

var knownRecordKinds = map[string]bool{
	kindDeliveryAccepted: true,
	kindCursorSaved:      true,
	kindJobCreated:       true,
	kindJobState:         true,
	kindJobThread:        true,
	kindJobPush:          true,
	kindDraftCreated:     true,
	kindDraftEdited:      true,
	kindDraftDecided:     true,
	kindDraftSending:     true,
	kindDraftReceipt:     true,
	kindDraftUncertain:   true,
	kindControlChanged:   true,
	kindBudgetReserved:   true,
}

type deliveryRecord struct {
	Source      string         `json:"source"`
	Key         string         `json:"key"`
	Digest      string         `json:"digest,omitempty"`
	Observation jsontext.Value `json:"observation,omitempty"`
	Envelope    jsontext.Value `json:"envelope,omitempty"`
}

type cursorRecord struct {
	Name  string      `json:"name"`
	Poll  *PollCursor `json:"poll,omitempty"`
	After string      `json:"after,omitempty"`
}

type controlRecord struct {
	Shutdown bool `json:"shutdown"`
}

type serviceRecord struct {
	Version  int             `json:"version"`
	Type     string          `json:"type"`
	Kind     string          `json:"kind"`
	At       time.Time       `json:"at"`
	Delivery *deliveryRecord `json:"delivery,omitempty"`
	Cursor   *cursorRecord   `json:"cursor,omitempty"`
	Control  *controlRecord  `json:"control,omitempty"`
}

type deliveryMeta struct {
	digest string
	at     time.Time
}

type serviceStore struct {
	journal    *journal.Store
	clock      func() time.Time
	damaged    error
	deliveries map[string]deliveryMeta
	poll       PollCursor
	hasPoll    bool
	relayAfter string
}

func openServiceStore(ctx context.Context, dir string, clock func() time.Time) (*serviceStore, error) {
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	j, err := journal.Open(dir)
	if err != nil {
		return nil, fmt.Errorf("daltoki: open service journal: %w", err)
	}
	if clock == nil {
		clock = time.Now
	}
	store := &serviceStore{journal: j, clock: clock, deliveries: map[string]deliveryMeta{}}
	if err := j.Replay(journalSession, func(_ uint64, ev []byte) error {
		return store.fold(ev)
	}); err != nil {
		_ = j.Close()
		return nil, fmt.Errorf("daltoki: open service journal: %w", err)
	}
	return store, nil
}

func (s *serviceStore) fold(ev []byte) error {
	var record serviceRecord
	if err := json.Unmarshal(ev, &record); err != nil {
		s.damaged = errJournalDamaged
		return nil
	}
	if record.Version != recordVersion || !knownRecordKinds[record.Kind] || record.At.IsZero() {
		s.damaged = errJournalDamaged
		return nil
	}
	s.foldRecord(record)
	return nil
}

func (s *serviceStore) foldRecord(record serviceRecord) {
	switch {
	case record.Delivery != nil:
		if record.Kind != kindDeliveryAccepted || record.Delivery.Key == "" {
			s.damaged = errJournalDamaged
			return
		}
		s.deliveries[record.Delivery.Key] = deliveryMeta{digest: record.Delivery.Digest, at: record.At}
	case record.Cursor != nil:
		s.foldCursor(record)
	case record.Control != nil:
		if record.Kind != kindControlChanged {
			s.damaged = errJournalDamaged
		}
	}
}

func (s *serviceStore) foldCursor(record serviceRecord) {
	if record.Kind != kindCursorSaved || record.Cursor.Name == "" {
		s.damaged = errJournalDamaged
		return
	}
	switch record.Cursor.Name {
	case "poll":
		if record.Cursor.Poll == nil {
			s.damaged = errJournalDamaged
			return
		}
		s.poll, s.hasPoll = *record.Cursor.Poll, true
	case "relay":
		s.relayAfter = record.Cursor.After
	default:
		s.damaged = errJournalDamaged
	}
}

func (s *serviceStore) append(ctx context.Context, record serviceRecord) error {
	if s.damaged != nil {
		return s.damaged
	}
	record.Type = record.Kind
	raw, err := json.Marshal(record)
	if err != nil {
		return err
	}
	_, err = s.journal.Append(ctx, journalSession, raw)
	return err
}

func (s *serviceStore) prune(now time.Time) {
	cutoff := now.Add(-deliveryTombstoneDays * 24 * time.Hour)
	for key, meta := range s.deliveries {
		if meta.at.Before(cutoff) {
			delete(s.deliveries, key)
		}
	}
}

func (s *serviceStore) Admit(ctx context.Context, observation Observation) (bool, error) {
	if s.damaged != nil {
		return false, s.damaged
	}
	key := "poll|" + observation.DedupKey()
	if _, seen := s.deliveries[key]; seen {
		return false, nil
	}
	body, err := json.Marshal(observation)
	if err != nil {
		return false, err
	}
	now := s.clock().UTC()
	s.prune(now)
	if err := s.append(ctx, serviceRecord{
		Version: recordVersion,
		Kind:    kindDeliveryAccepted,
		At:      now,
		Delivery: &deliveryRecord{
			Source:      "poll",
			Key:         key,
			Observation: body,
		},
	}); err != nil {
		return false, err
	}
	s.deliveries[key] = deliveryMeta{at: now}
	return true, nil
}

func (s *serviceStore) AdmitEnvelope(ctx context.Context, envelope Envelope) (bool, error) {
	if s.damaged != nil {
		return false, s.damaged
	}
	key := "relay|" + envelope.DeliveryID
	if meta, seen := s.deliveries[key]; seen {
		if meta.digest != envelope.BodySHA256 {
			return false, errDeliveryConflict
		}
		return false, nil
	}
	body, err := json.Marshal(envelope)
	if err != nil {
		return false, err
	}
	now := s.clock().UTC()
	s.prune(now)
	if err := s.append(ctx, serviceRecord{
		Version: recordVersion,
		Kind:    kindDeliveryAccepted,
		At:      now,
		Delivery: &deliveryRecord{
			Source:   "relay",
			Key:      key,
			Digest:   envelope.BodySHA256,
			Envelope: body,
		},
	}); err != nil {
		return false, err
	}
	s.deliveries[key] = deliveryMeta{digest: envelope.BodySHA256, at: now}
	return true, nil
}

func (s *serviceStore) LoadCursor(ctx context.Context) (PollCursor, bool, error) {
	if s.damaged != nil {
		return PollCursor{}, false, s.damaged
	}
	return s.poll, s.hasPoll, ctx.Err()
}

func (s *serviceStore) SaveCursor(ctx context.Context, cursor PollCursor) error {
	now := s.clock().UTC()
	if err := s.append(ctx, serviceRecord{
		Version: recordVersion,
		Kind:    kindCursorSaved,
		At:      now,
		Cursor:  &cursorRecord{Name: "poll", Poll: &cursor},
	}); err != nil {
		return err
	}
	s.poll, s.hasPoll = cursor, true
	return nil
}

func (s *serviceStore) LoadRelayAfter() string {
	return s.relayAfter
}

func (s *serviceStore) SaveRelayAfter(ctx context.Context, after string) error {
	now := s.clock().UTC()
	if err := s.append(ctx, serviceRecord{
		Version: recordVersion,
		Kind:    kindCursorSaved,
		At:      now,
		Cursor:  &cursorRecord{Name: "relay", After: after},
	}); err != nil {
		return err
	}
	s.relayAfter = after
	return nil
}

func (s *serviceStore) RecordControl(ctx context.Context, control controlRecord) error {
	return s.append(ctx, serviceRecord{
		Version: recordVersion,
		Kind:    kindControlChanged,
		At:      s.clock().UTC(),
		Control: &control,
	})
}

func (s *serviceStore) Close() error {
	return s.journal.Close()
}
