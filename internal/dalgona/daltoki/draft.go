package daltoki

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

const reconcilePages = 10

var (
	errPublicationClosed = errors.New("daltoki: publication is paused until the service resumes")
	errMissingReceipt    = errors.New("daltoki: publication returned no receipt")

	ErrUnknownDraft    = errors.New("daltoki: unknown draft")
	ErrDraftState      = errors.New("daltoki: draft state does not allow this change")
	ErrEmptyDraft      = errors.New("daltoki: draft body is empty")
	ErrServiceDisabled = errors.New("daltoki: service is disabled")
	ErrServiceStopped  = errors.New("daltoki: service is not running")
)

type StaleDraftError struct {
	Current string
}

func (e *StaleDraftError) Error() string {
	return "daltoki: draft changed since it was read"
}

type DraftKind string

const (
	DraftComment     DraftKind = "comment"
	DraftPullRequest DraftKind = "pull-request"
)

type DraftState string

const (
	DraftPending   DraftState = "pending"
	DraftApproved  DraftState = "approved"
	DraftRejected  DraftState = "rejected"
	DraftSending   DraftState = "sending"
	DraftUncertain DraftState = "uncertain"
	DraftPosted    DraftState = "posted"
)

var draftSources = map[string][]DraftState{
	kindDraftEdited:    {DraftPending, DraftApproved},
	kindDraftDecided:   {DraftPending, DraftUncertain},
	kindDraftSending:   {DraftApproved},
	kindDraftUncertain: {DraftSending},
	kindDraftReceipt:   {DraftSending, DraftUncertain},
}

type Draft struct {
	ID         string
	JobID      string
	Kind       DraftKind
	Repository github.Repository
	Issue      int64
	Title      string
	Body       string
	Head       string
	Base       string
	Commit     string
	State      DraftState
	Reason     string
	Sent       bool
	SendingAt  time.Time
	ReceiptID  int64
	URL        string
}

type draftRecord struct {
	ID        string     `json:"id"`
	JobID     string     `json:"jobId,omitempty"`
	Kind      DraftKind  `json:"kind,omitempty"`
	Title     string     `json:"title,omitempty"`
	Body      string     `json:"body,omitempty"`
	Head      string     `json:"head,omitempty"`
	Base      string     `json:"base,omitempty"`
	Commit    string     `json:"commit,omitempty"`
	Decision  DraftState `json:"decision,omitempty"`
	Reason    string     `json:"reason,omitempty"`
	ReceiptID int64      `json:"receiptId,omitempty"`
	URL       string     `json:"url,omitempty"`
}

type draftReceipt struct {
	ID  int64
	URL string
}

func draftID(job string) string {
	return "d-" + strings.TrimPrefix(job, "j-")
}

func draftMarker(id string) string {
	return "<!-- daltoki:draft:" + id + " -->"
}

func (d Draft) publishedBody() string {
	return d.Body + "\n\n" + draftMarker(d.ID)
}

func (s *serviceStore) checkDraft(kind string, record draftRecord) error {
	if kind == kindDraftCreated {
		return s.checkDraftCreated(record)
	}
	draft, ok := s.drafts[record.ID]
	if !ok || !draftPayloadValid(kind, record) {
		return errJournalDamaged
	}
	if kind == kindDraftReceipt && draft.State == DraftApproved && draft.Sent {
		return nil
	}
	if !slices.Contains(draftSources[kind], draft.State) {
		return errJournalDamaged
	}
	return nil
}

func draftPayloadValid(kind string, record draftRecord) bool {
	switch kind {
	case kindDraftEdited:
		return strings.TrimSpace(record.Body) != ""
	case kindDraftDecided:
		return record.Decision == DraftApproved || record.Decision == DraftRejected
	case kindDraftReceipt:
		return record.ReceiptID > 0
	}
	return true
}

func draftShapeValid(record draftRecord) bool {
	switch record.Kind {
	case DraftComment:
		return true
	case DraftPullRequest:
		return record.Title != "" && record.Head != "" && record.Base != "" && record.Commit != ""
	}
	return false
}

func (s *serviceStore) checkDraftCreated(record draftRecord) error {
	_, exists := s.drafts[record.ID]
	job, known := s.jobs[record.JobID]
	if exists || !known || record.ID == "" || job.State != JobRunning || strings.TrimSpace(record.Body) == "" || !draftShapeValid(record) {
		return errJournalDamaged
	}
	return nil
}

func (s *serviceStore) applyDraft(kind string, record draftRecord, at time.Time) {
	if kind == kindDraftCreated {
		job := s.jobs[record.JobID]
		s.drafts[record.ID] = &Draft{
			ID: record.ID, JobID: record.JobID, Kind: record.Kind, Repository: job.Repository, Issue: job.Issue,
			Title: record.Title, Body: record.Body, Head: record.Head, Base: record.Base, Commit: record.Commit,
			State: DraftPending,
		}
		s.draftOrder = append(s.draftOrder, record.ID)
		return
	}
	draft := s.drafts[record.ID]
	switch kind {
	case kindDraftEdited:
		draft.Title, draft.Body, draft.State = cmp.Or(record.Title, draft.Title), record.Body, DraftPending
	case kindDraftDecided:
		draft.State = record.Decision
	case kindDraftSending:
		draft.State, draft.Sent, draft.SendingAt = DraftSending, true, at
	case kindDraftUncertain:
		draft.State, draft.Reason = DraftUncertain, record.Reason
	case kindDraftReceipt:
		draft.State, draft.ReceiptID, draft.URL = DraftPosted, record.ReceiptID, record.URL
	}
}

func (s *serviceStore) Drafts() []Draft {
	s.mu.Lock()
	defer s.mu.Unlock()
	drafts := make([]Draft, 0, len(s.draftOrder))
	for _, id := range s.draftOrder {
		drafts = append(drafts, *s.drafts[id])
	}
	return drafts
}

func (s *serviceStore) draftRecordOf(kind string, record draftRecord) serviceRecord {
	out := s.record(kind, s.clock().UTC())
	out.Draft = &record
	return out
}

func (s *serviceStore) closeJobRecords(jobID, reason string) []serviceRecord {
	if job, ok := s.jobs[jobID]; ok && job.State == JobWaitingApproval {
		return []serviceRecord{s.jobStateRecord(jobID, JobCompleted, reason, nil)}
	}
	return nil
}

func (d Draft) Revision() string {
	sum := sha256.New()
	for _, part := range []string{string(d.State), d.Title, d.Body} {
		_, _ = sum.Write([]byte(part))
		_, _ = sum.Write([]byte{0})
	}
	return hex.EncodeToString(sum.Sum(nil))
}

func (s *serviceStore) changeable(id, revision, kind string) (*Draft, error) {
	draft, ok := s.drafts[id]
	if !ok {
		return nil, ErrUnknownDraft
	}
	if current := draft.Revision(); current != revision {
		return nil, &StaleDraftError{Current: current}
	}
	if !slices.Contains(draftSources[kind], draft.State) {
		return nil, ErrDraftState
	}
	return draft, nil
}

func (s *serviceStore) EditDraft(ctx context.Context, id, revision, title, body string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if strings.TrimSpace(body) == "" {
		return ErrEmptyDraft
	}
	if _, err := s.changeable(id, revision, kindDraftEdited); err != nil {
		return err
	}
	return s.commit(ctx, s.draftRecordOf(kindDraftEdited, draftRecord{ID: id, Title: title, Body: body}))
}

func (s *serviceStore) DecideDraft(ctx context.Context, id, revision string, decision DraftState) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	draft, err := s.changeable(id, revision, kindDraftDecided)
	if err != nil {
		return err
	}
	records := []serviceRecord{s.draftRecordOf(kindDraftDecided, draftRecord{ID: id, Decision: decision})}
	if decision == DraftRejected {
		records = append(records, s.closeJobRecords(draft.JobID, "daltoki: draft was rejected")...)
	}
	return s.commit(ctx, records...)
}

func (s *serviceStore) MarkSending(ctx context.Context, id, revision string) (*Draft, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	draft, ok := s.drafts[id]
	if !ok {
		return nil, ErrUnknownDraft
	}
	if current := draft.Revision(); current != revision {
		return nil, &StaleDraftError{Current: current}
	}
	if draft.State != DraftApproved {
		return nil, ErrDraftState
	}
	job, ok := s.jobs[draft.JobID]
	if !ok || job.State != JobWaitingApproval {
		return nil, ErrDraftState
	}
	if err := s.commit(ctx, s.draftRecordOf(kindDraftSending, draftRecord{ID: id})); err != nil {
		return nil, err
	}
	admitted := *s.drafts[id]
	return &admitted, nil
}

func (s *serviceStore) MarkUncertain(ctx context.Context, id, reason string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.commit(ctx, s.draftRecordOf(kindDraftUncertain, draftRecord{ID: id, Reason: reason}))
}

func (s *serviceStore) MarkPosted(ctx context.Context, id string, receipt draftReceipt) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	draft, ok := s.drafts[id]
	if !ok {
		return ErrUnknownDraft
	}
	records := []serviceRecord{s.draftRecordOf(kindDraftReceipt, draftRecord{ID: id, ReceiptID: receipt.ID, URL: receipt.URL})}
	records = append(records, s.closeJobRecords(draft.JobID, "")...)
	return s.commit(ctx, records...)
}

func (s *serviceStore) RecordPush(ctx context.Context, jobID string, push JobPush) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	record := s.record(kindJobPush, s.clock().UTC())
	record.Job = &jobRecord{ID: jobID, Branch: push.Branch, Commit: push.Commit}
	return s.commit(ctx, record)
}

func (s *serviceStore) Publishable() []Draft {
	s.mu.Lock()
	defer s.mu.Unlock()
	var drafts []Draft
	for _, id := range s.draftOrder {
		draft := s.drafts[id]
		if draft.State == DraftApproved && s.jobs[draft.JobID].State == JobWaitingApproval {
			drafts = append(drafts, *draft)
		}
	}
	return drafts
}

func (s *Service) Jobs() []Job {
	if s.store == nil {
		return nil
	}
	return s.store.Jobs()
}

func (s *Service) Drafts() []Draft {
	if s.store == nil {
		return nil
	}
	return s.store.Drafts()
}

func (s *Service) EditDraft(ctx context.Context, id, revision, title, body string) error {
	if s.store == nil {
		return ErrServiceDisabled
	}
	return s.store.EditDraft(ctx, id, revision, title, body)
}

func (s *Service) DecideDraft(ctx context.Context, id, revision string, approve bool) error {
	if s.store == nil {
		return ErrServiceDisabled
	}
	decision := DraftRejected
	if approve {
		decision = DraftApproved
	}
	if err := s.store.DecideDraft(ctx, id, revision, decision); err != nil {
		return err
	}
	s.wake()
	return nil
}

func (s *Service) Resume(ctx context.Context) error {
	if s.store == nil {
		return ErrServiceDisabled
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed || !s.running {
		return ErrServiceStopped
	}
	if s.store.ShutdownHeld() {
		if err := s.store.RecordControl(ctx, controlRecord{Shutdown: false}); err != nil {
			return err
		}
	}
	s.publishing = true
	s.wake()
	return nil
}

func (s *Service) publicationOpen() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.publishing
}

func (s *Service) admitPublication(record func() error) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.publishing {
		return errPublicationClosed
	}
	return record()
}

func (s *Service) admitDraft(ctx context.Context, draft Draft) (*Draft, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.publishing {
		return nil, errPublicationClosed
	}
	return s.store.MarkSending(ctx, draft.ID, draft.Revision())
}

func (s *Service) publishApproved(ctx context.Context) {
	for _, draft := range s.store.Publishable() {
		if ctx.Err() != nil {
			return
		}
		if err := s.publish(ctx, draft); err != nil && !errors.Is(err, errPublicationClosed) {
			s.deps.Logger.Error("daltoki: draft publication failed", "draft", draft.ID, "error", err)
		}
	}
}

func (s *Service) publish(ctx context.Context, draft Draft) error {
	if !s.publicationOpen() {
		return errPublicationClosed
	}
	settled, err := s.settledSentDraft(ctx, draft)
	if err != nil {
		return err
	}
	if settled {
		return nil
	}
	if draft.Kind == DraftPullRequest {
		pushErr := s.pushDraft(ctx, draft)
		if pushErr != nil {
			return pushErr
		}
	}
	admitted, err := s.admitDraft(ctx, draft)
	if err != nil {
		return err
	}
	resettled, err := s.settledSentDraft(ctx, *admitted)
	if err != nil {
		return err
	}
	if resettled {
		return nil
	}
	return s.sendAdmitted(ctx, *admitted)
}

func (s *Service) settledSentDraft(ctx context.Context, draft Draft) (bool, error) {
	if !draft.Sent {
		return false, nil
	}
	return s.reconcile(ctx, draft)
}

func (s *Service) sendAdmitted(ctx context.Context, admitted Draft) error {
	receipt, err := s.send(ctx, admitted)
	settle := context.WithoutCancel(ctx)
	if err != nil {
		return errors.Join(err, s.store.MarkUncertain(settle, admitted.ID, err.Error()))
	}
	return s.store.MarkPosted(settle, admitted.ID, receipt)
}

func (s *Service) pushDraft(ctx context.Context, draft Draft) error {
	job, ok := s.store.Job(draft.JobID)
	if !ok {
		return errJournalDamaged
	}
	push := JobPush{Branch: draft.Head, Commit: draft.Commit}
	if job.Push == nil || *job.Push != push {
		if err := s.admitPublication(func() error { return s.store.RecordPush(ctx, job.ID, push) }); err != nil {
			return err
		}
	}
	err := s.deps.Worktrees.Push(ctx, PushIntent{Repository: job.Repository, Branch: push.Branch, Commit: push.Commit})
	if err == nil || ctx.Err() != nil {
		return err
	}
	return errors.Join(err, s.store.MoveJob(context.WithoutCancel(ctx), job.ID, JobFailed, err.Error(), nil))
}

func (s *Service) send(ctx context.Context, draft Draft) (draftReceipt, error) {
	var receipt draftReceipt
	var err error
	switch draft.Kind {
	case DraftComment:
		var comment github.Comment
		comment, _, err = s.deps.Client.CreateComment(ctx, github.CreateCommentRequest{
			Repository: draft.Repository, Number: draft.Issue, Body: draft.publishedBody(),
		})
		receipt = draftReceipt{ID: comment.ID, URL: comment.HTMLURL}
	case DraftPullRequest:
		var pull github.PullRequest
		pull, _, err = s.deps.Client.CreatePullRequest(ctx, github.CreatePullRequestRequest{
			Repository: draft.Repository, Title: draft.Title, Body: draft.publishedBody(),
			Head: draft.Head, Base: draft.Base, Draft: true,
		})
		receipt = draftReceipt{ID: pull.ID, URL: pull.HTMLURL}
	}
	if err == nil && receipt.ID == 0 {
		err = errMissingReceipt
	}
	return receipt, err
}

func (s *Service) reconcile(ctx context.Context, draft Draft) (bool, error) {
	receipt, found, err := s.findPublished(ctx, draft)
	if err != nil || !found {
		return false, err
	}
	return true, s.store.MarkPosted(ctx, draft.ID, receipt)
}

func (s *Service) findPublished(ctx context.Context, draft Draft) (draftReceipt, bool, error) {
	if draft.Kind == DraftPullRequest {
		return s.findPullRequest(ctx, draft)
	}
	return s.findComment(ctx, draft)
}

func scanPages[T any](fetch func(next string) ([]T, string, error), match func(T) (draftReceipt, bool)) (draftReceipt, bool, error) {
	next := ""
	for range reconcilePages {
		items, following, err := fetch(next)
		if err != nil {
			return draftReceipt{}, false, fmt.Errorf("daltoki: reconcile draft: %w", err)
		}
		for _, item := range items {
			if receipt, ok := match(item); ok {
				return receipt, true, nil
			}
		}
		if following == "" {
			return draftReceipt{}, false, nil
		}
		next = following
	}
	return draftReceipt{}, false, fmt.Errorf("daltoki: reconcile draft: history exceeds %d pages", reconcilePages)
}

func (s *Service) findComment(ctx context.Context, draft Draft) (draftReceipt, bool, error) {
	marker := draftMarker(draft.ID)
	since := draft.SendingAt.Add(-pollOverlap)
	return scanPages(func(next string) ([]github.Comment, string, error) {
		page, meta, err := s.deps.Client.Comments(ctx, github.CommentsRequest{
			Repository: draft.Repository, Number: draft.Issue, Since: since, NextURL: next,
		})
		return page.Items, meta.NextURL, err
	}, func(comment github.Comment) (draftReceipt, bool) {
		return draftReceipt{ID: comment.ID, URL: comment.HTMLURL}, strings.Contains(comment.Body, marker)
	})
}

func (s *Service) findPullRequest(ctx context.Context, draft Draft) (draftReceipt, bool, error) {
	return scanPages(func(next string) ([]github.PullRequest, string, error) {
		page, meta, err := s.deps.Client.PullRequests(ctx, github.PullRequestsRequest{
			Repository: draft.Repository, Head: draft.Head, NextURL: next,
		})
		return page.Items, meta.NextURL, err
	}, func(pull github.PullRequest) (draftReceipt, bool) {
		return draftReceipt{ID: pull.ID, URL: pull.HTMLURL}, pull.Head.Ref == draft.Head
	})
}
