package daltoki

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"sync"
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

type payloadKind int

const (
	payloadDelivery payloadKind = iota + 1
	payloadCursor
	payloadJob
	payloadDraft
	payloadControl
	payloadBudget
)

var recordPayloads = map[string]payloadKind{
	kindDeliveryAccepted: payloadDelivery,
	kindCursorSaved:      payloadCursor,
	kindJobCreated:       payloadJob,
	kindJobState:         payloadJob,
	kindJobThread:        payloadJob,
	kindJobPush:          payloadJob,
	kindDraftCreated:     payloadDraft,
	kindDraftEdited:      payloadDraft,
	kindDraftDecided:     payloadDraft,
	kindDraftSending:     payloadDraft,
	kindDraftReceipt:     payloadDraft,
	kindDraftUncertain:   payloadDraft,
	kindControlChanged:   payloadControl,
	kindBudgetReserved:   payloadBudget,
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
	Job      *jobRecord      `json:"job,omitempty"`
	Draft    *draftRecord    `json:"draft,omitempty"`
	Cursor   *cursorRecord   `json:"cursor,omitempty"`
	Control  *controlRecord  `json:"control,omitempty"`
	Budget   *budgetRecord   `json:"budget,omitempty"`
}

func (r serviceRecord) payload() (payloadKind, int) {
	present := [...]struct {
		set  bool
		kind payloadKind
	}{
		{r.Delivery != nil, payloadDelivery},
		{r.Cursor != nil, payloadCursor},
		{r.Job != nil, payloadJob},
		{r.Draft != nil, payloadDraft},
		{r.Control != nil, payloadControl},
		{r.Budget != nil, payloadBudget},
	}
	var kind payloadKind
	count := 0
	for _, entry := range present {
		if entry.set {
			kind = entry.kind
			count++
		}
	}
	return kind, count
}

func (r serviceRecord) wellFormed() bool {
	want, known := recordPayloads[r.Kind]
	if !known || r.Version != recordVersion || r.At.IsZero() {
		return false
	}
	kind, count := r.payload()
	return count == 1 && kind == want
}

type deliveryMeta struct {
	digest string
	at     time.Time
}

type serviceStore struct {
	mu         sync.Mutex
	journal    *journal.Store
	clock      func() time.Time
	damaged    error
	firstAt    time.Time
	deliveries map[string]deliveryMeta
	poll       PollCursor
	hasPoll    bool
	relayAfter string
	shutdown   bool
	jobs       map[string]*Job
	jobOrder   []string
	issueJobs  map[string]string
	drafts     map[string]*Draft
	draftOrder []string
	budgets    map[string]budgetRecord
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
	store := &serviceStore{
		journal:    j,
		clock:      clock,
		deliveries: map[string]deliveryMeta{},
		jobs:       map[string]*Job{},
		issueJobs:  map[string]string{},
		drafts:     map[string]*Draft{},
		budgets:    map[string]budgetRecord{},
	}
	if err := j.Replay(journalSession, func(_ uint64, ev []byte) error {
		return store.fold(ev)
	}); err != nil {
		_ = j.Close()
		return nil, fmt.Errorf("daltoki: open service journal: %w", err)
	}
	if store.firstAt.IsZero() {
		store.firstAt = clock().UTC()
	}
	return store, nil
}

func (s *serviceStore) fold(ev []byte) error {
	if s.damaged != nil {
		return nil
	}
	var record serviceRecord
	if err := json.Unmarshal(ev, &record); err != nil {
		s.damaged = errJournalDamaged
		return nil
	}
	if err := s.check(record); err != nil {
		s.damaged = errJournalDamaged
		return nil
	}
	if s.firstAt.IsZero() {
		s.firstAt = record.At
	}
	s.apply(record)
	return nil
}

func (s *serviceStore) check(record serviceRecord) error {
	if !record.wellFormed() {
		return errJournalDamaged
	}
	switch {
	case record.Delivery != nil:
		if record.Delivery.Key == "" {
			return errJournalDamaged
		}
	case record.Cursor != nil:
		return checkCursor(*record.Cursor)
	case record.Job != nil:
		return s.checkJob(record.Kind, *record.Job)
	case record.Draft != nil:
		return s.checkDraft(record.Kind, *record.Draft)
	case record.Budget != nil:
		return s.checkBudget(*record.Budget)
	}
	return nil
}

func checkCursor(cursor cursorRecord) error {
	switch {
	case cursor.Name == "poll" && cursor.Poll != nil:
		return nil
	case cursor.Name == "relay":
		return nil
	}
	return errJournalDamaged
}

func (s *serviceStore) apply(record serviceRecord) {
	switch {
	case record.Delivery != nil:
		s.deliveries[record.Delivery.Key] = deliveryMeta{digest: record.Delivery.Digest, at: record.At}
	case record.Cursor != nil:
		s.applyCursor(*record.Cursor)
	case record.Job != nil:
		s.applyJob(record.Kind, *record.Job)
	case record.Draft != nil:
		s.applyDraft(record.Kind, *record.Draft, record.At)
	case record.Control != nil:
		s.shutdown = record.Control.Shutdown
	case record.Budget != nil:
		s.budgets[record.Budget.JobID] = *record.Budget
	}
}

func (s *serviceStore) applyCursor(cursor cursorRecord) {
	if cursor.Name == "poll" {
		s.poll, s.hasPoll = *cursor.Poll, true
		return
	}
	s.relayAfter = cursor.After
}

func (s *serviceStore) record(kind string, at time.Time) serviceRecord {
	return serviceRecord{Version: recordVersion, Type: kind, Kind: kind, At: at}
}

func (s *serviceStore) commit(ctx context.Context, records ...serviceRecord) error {
	if s.damaged != nil {
		return s.damaged
	}
	raws := make([][]byte, 0, len(records))
	for i := range records {
		records[i].Type = records[i].Kind
		if err := s.check(records[i]); err != nil {
			return err
		}
		raw, err := json.Marshal(records[i])
		if err != nil {
			return fmt.Errorf("daltoki: encode %s: %w", records[i].Kind, err)
		}
		raws = append(raws, raw)
	}
	if _, err := s.journal.Append(ctx, journalSession, raws...); err != nil {
		return fmt.Errorf("daltoki: append service record: %w", err)
	}
	for _, record := range records {
		s.apply(record)
	}
	return nil
}

func (s *serviceStore) prune(now time.Time) {
	cutoff := now.Add(-deliveryTombstoneDays * 24 * time.Hour)
	for key, meta := range s.deliveries {
		if meta.at.Before(cutoff) {
			delete(s.deliveries, key)
		}
	}
}

func (s *serviceStore) Damaged() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.damaged
}

func (s *serviceStore) FirstAt() time.Time {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.firstAt
}

func (s *serviceStore) seen(key, digest string) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.damaged != nil {
		return false, s.damaged
	}
	meta, seen := s.deliveries[key]
	if seen && digest != "" && meta.digest != digest {
		return true, errDeliveryConflict
	}
	return seen, nil
}

func (s *serviceStore) accept(ctx context.Context, delivery deliveryRecord, job *jobRecord) (bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.damaged != nil {
		return false, s.damaged
	}
	if meta, seen := s.deliveries[delivery.Key]; seen {
		if delivery.Digest != "" && meta.digest != delivery.Digest {
			return false, errDeliveryConflict
		}
		return false, nil
	}
	now := s.clock().UTC()
	s.prune(now)
	accepted := s.record(kindDeliveryAccepted, now)
	accepted.Delivery = &delivery
	records := []serviceRecord{accepted}
	if job != nil && !s.issueBusy(job.Kind, job.Repository, job.Issue) {
		created := s.record(kindJobCreated, now)
		created.Job = job
		records = append(records, created)
	}
	if err := s.commit(ctx, records...); err != nil {
		return false, err
	}
	return true, nil
}

func (s *serviceStore) LoadCursor(ctx context.Context) (PollCursor, bool, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.damaged != nil {
		return PollCursor{}, false, s.damaged
	}
	return s.poll, s.hasPoll, ctx.Err()
}

func (s *serviceStore) SaveCursor(ctx context.Context, cursor PollCursor) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	record := s.record(kindCursorSaved, s.clock().UTC())
	record.Cursor = &cursorRecord{Name: "poll", Poll: &cursor}
	return s.commit(ctx, record)
}

func (s *serviceStore) LoadRelayAfter() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.relayAfter
}

func (s *serviceStore) SaveRelayAfter(ctx context.Context, after string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	record := s.record(kindCursorSaved, s.clock().UTC())
	record.Cursor = &cursorRecord{Name: "relay", After: after}
	return s.commit(ctx, record)
}

func (s *serviceStore) ShutdownHeld() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.shutdown
}

func (s *serviceStore) RecordControl(ctx context.Context, control controlRecord) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	record := s.record(kindControlChanged, s.clock().UTC())
	record.Control = &control
	return s.commit(ctx, record)
}

func (s *serviceStore) Close() error {
	return s.journal.Close()
}
