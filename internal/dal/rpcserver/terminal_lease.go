package rpcserver

import (
	"crypto/sha256"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	accessRead  = "read"
	accessWrite = "write"
)

type attachment struct {
	id          string
	owner       *server
	workspaceID string
	release     func()
	validate    func() error
	writer      *writerLease
	deadline    time.Time
	checkpoint  *checkpoint
}

type writerLease struct {
	secret string
	holder *attachment
	input  sequenceLog
	resize sequenceLog
}

type sequenceState int

const (
	sequencePending sequenceState = iota
	sequenceDone
	sequenceUnknown
)

type sequenceRecord struct {
	seq    uint64
	digest [sha256.Size]byte
	state  sequenceState
	resize terminalResize
}

type sequenceLog struct {
	accepted uint64
	last     uint64
	records  []sequenceRecord
}

func (l *sequenceLog) find(seq uint64) *sequenceRecord {
	if len(l.records) == 0 || seq < l.records[0].seq {
		return nil
	}
	index := seq - l.records[0].seq
	if index >= uint64(len(l.records)) {
		return nil
	}
	return &l.records[index]
}

func (l *sequenceLog) repeated(seq uint64, digest [sha256.Size]byte) (sequenceRecord, bool, error) {
	if seq == 0 {
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if seq > l.last+1 {
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorOperationConflict)
	}
	if seq == l.last+1 {
		return sequenceRecord{}, false, nil
	}
	record := l.find(seq)
	switch {
	case record == nil:
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorExpired)
	case record.digest != digest:
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorOperationConflict)
	case record.state == sequencePending:
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorBusy)
	case record.state == sequenceUnknown:
		return sequenceRecord{}, true, rpc.NativeKind(rpc.ErrorUnknownOutcome)
	}
	return *record, true, nil
}

func (l *sequenceLog) reserve(seq uint64, digest [sha256.Size]byte) {
	l.records = append(l.records, sequenceRecord{seq: seq, digest: digest})
	if len(l.records) > terminalSequenceMemory {
		l.records = l.records[len(l.records)-terminalSequenceMemory:]
	}
	l.last = seq
}

func (l *sequenceLog) settle(seq uint64, state sequenceState, resize terminalResize) {
	record := l.find(seq)
	if record == nil {
		return
	}
	record.state = state
	record.resize = resize
	if state == sequenceDone {
		l.accepted = seq
	}
}

func (t *terminal) leaseDeadline(candidate time.Time) time.Time {
	if t.final != nil && candidate.After(t.final.expiresAt) {
		return t.final.expiresAt
	}
	return candidate
}

func (t *terminal) renewLocked(att *attachment, now time.Time) {
	att.deadline = t.leaseDeadline(now.Add(terminalLease))
	t.stopAt = t.latestDeadlineLocked()
	t.scheduleLocked(now)
	att.owner.workspaceTouch(att.workspaceID)
}

func (t *terminal) latestDeadlineLocked() time.Time {
	var latest time.Time
	for _, att := range t.attachments {
		if att.deadline.After(latest) {
			latest = att.deadline
		}
	}
	return latest
}

func (t *terminal) attachedBy(s *server) bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	for _, att := range t.attachments {
		if att.owner == s {
			return true
		}
	}
	return false
}

func (t *terminal) ownedLocked(s *server, attachmentID string, now time.Time) (*attachment, error) {
	att, ok := t.attachments[attachmentID]
	if t.released || !ok || att.owner != s || !now.Before(att.deadline) {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	return att, nil
}

func (t *terminal) writerLocked(s *server, secret string, now time.Time) (*writerLease, error) {
	if t.released || t.inputState == inputClosed {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	lease := t.writer
	if lease == nil || lease.secret != secret || lease.holder == nil || lease.holder.owner != s || !now.Before(lease.holder.deadline) {
		return nil, rpc.NativeKind(rpc.ErrorPermissionDenied)
	}
	return lease, nil
}

type terminalAccess struct {
	owner        *server
	attachmentID string
	writerSecret string
}

func (a terminalAccess) lookupLocked(t *terminal) (*attachment, error) {
	now := time.Now()
	if a.writerSecret == "" {
		return t.ownedLocked(a.owner, a.attachmentID, now)
	}
	lease, err := t.writerLocked(a.owner, a.writerSecret, now)
	if err != nil {
		return nil, err
	}
	return lease.holder, nil
}

func (t *terminal) lockAttachment(s *server, id string) (*attachment, error) {
	return t.lockAccess(terminalAccess{owner: s, attachmentID: id})
}

func (t *terminal) lockWriter(s *server, secret string) (*writerLease, error) {
	att, err := t.lockAccess(terminalAccess{owner: s, writerSecret: secret})
	if err != nil {
		return nil, err
	}
	return att.writer, nil
}

func (t *terminal) lockAccess(access terminalAccess) (*attachment, error) {
	t.mu.Lock()
	for {
		candidate, err := access.lookupLocked(t)
		if err != nil {
			t.mu.Unlock()
			return nil, err
		}
		t.mu.Unlock()
		authorityErr := candidate.validate()
		t.mu.Lock()
		current, err := access.lookupLocked(t)
		if err != nil {
			t.mu.Unlock()
			return nil, err
		}
		if current != candidate {
			continue
		}
		if authorityErr != nil {
			t.mu.Unlock()
			return nil, authorityErr
		}
		return current, nil
	}
}

type attachRequest struct {
	owner       *server
	workspaceID string
	release     func()
	validate    func() error
	access      string
	resumeLease string
}

func (t *terminal) attachLocked(req attachRequest, now time.Time) (*attachment, *attachment, error) {
	if t.released {
		return nil, nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	resumed, err := t.resumedHolderLocked(req)
	if err != nil {
		return nil, nil, err
	}
	free := terminalAttachmentLimit - len(t.attachments)
	if resumed != nil {
		free++
	}
	if free <= 0 {
		return nil, nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	cp, err := t.captureLocked(now)
	if err != nil {
		return nil, nil, err
	}
	retained := t.writer
	if resumed != nil {
		t.removeAttachmentLocked(resumed)
	}
	att := &attachment{id: newHandle(), owner: req.owner, workspaceID: req.workspaceID, release: req.release, validate: req.validate, checkpoint: cp}
	if req.access == accessWrite {
		att.writer = t.grantWriterLocked(retained, resumed != nil)
		att.writer.holder = att
	}
	t.attachments[att.id] = att
	t.renewLocked(att, now)
	return att, resumed, nil
}

func (t *terminal) resumedHolderLocked(req attachRequest) (*attachment, error) {
	if req.access != accessWrite {
		return t.resumedReader(req)
	}
	return t.resumedWriter(req)
}

func (t *terminal) resumedReader(req attachRequest) (*attachment, error) {
	if req.resumeLease == "" || t.writer == nil || t.writer.secret != req.resumeLease {
		return nil, nil
	}
	return t.writer.holder, nil
}

func (t *terminal) resumedWriter(req attachRequest) (*attachment, error) {
	if t.inputState == inputClosed {
		return nil, rpc.NativeKind(rpc.ErrorPermissionDenied)
	}
	resumes := req.resumeLease != "" && t.writer != nil && t.writer.secret == req.resumeLease
	switch {
	case req.resumeLease == "" && t.writer != nil:
		return nil, rpc.NativeKind(rpc.ErrorBusy)
	case req.resumeLease != "" && !resumes:
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	case resumes:
		return t.writer.holder, nil
	}
	return nil, nil
}

func (t *terminal) grantWriterLocked(retained *writerLease, resumed bool) *writerLease {
	if resumed {
		t.writer = retained
		return retained
	}
	t.writer = &writerLease{secret: newHandle()}
	return t.writer
}

func (t *terminal) removeAttachmentLocked(att *attachment) {
	delete(t.attachments, att.id)
	t.stopAt = att.deadline
	if len(t.attachments) > 0 {
		t.stopAt = t.latestDeadlineLocked()
	}
	t.retireCheckpoint(att)
	if att.writer != nil && t.writer == att.writer {
		t.writer.holder = nil
		t.writer = nil
	}
	att.writer = nil
}

func (t *terminal) dropAttachmentsLocked(expired func(*attachment) bool) []*attachment {
	var dropped []*attachment
	for _, att := range t.attachments {
		if expired(att) {
			t.removeAttachmentLocked(att)
			dropped = append(dropped, att)
		}
	}
	return dropped
}

func releaseWorkspaces(dropped []*attachment) {
	for _, att := range dropped {
		if att.release != nil {
			att.release()
		}
	}
}

func (t *terminal) detach(s *server, attachmentID string) string {
	t.mu.Lock()
	att, ok := t.attachments[attachmentID]
	if t.released || !ok || att.owner != s {
		t.mu.Unlock()
		return "notFound"
	}
	t.removeAttachmentLocked(att)
	retire := t.final != nil && len(t.attachments) == 0
	t.scheduleLocked(time.Now())
	t.mu.Unlock()
	releaseWorkspaces([]*attachment{att})
	if retire {
		t.release()
	}
	return "detached"
}

func (t *terminal) expire() {
	now := time.Now()
	t.mu.Lock()
	if t.released {
		t.mu.Unlock()
		return
	}
	dropped := t.dropAttachmentsLocked(func(att *attachment) bool { return !now.Before(att.deadline) })
	t.sweepCheckpointsLocked(now)
	stop := t.state == terminalRunning && len(t.attachments) == 0 && !now.Before(t.stopAt)
	retire := t.final != nil && !now.Before(t.final.expiresAt)
	t.scheduleLocked(now)
	t.mu.Unlock()
	releaseWorkspaces(dropped)
	if retire {
		t.release()
		return
	}
	if stop {
		t.requestClose()
	}
}

func (t *terminal) sweepCheckpointsLocked(now time.Time) {
	for _, att := range t.attachments {
		if att.checkpoint == nil || now.Before(att.checkpoint.expiresAt) {
			continue
		}
		t.retireCheckpoint(att)
	}
}

func (t *terminal) scheduleLocked(now time.Time) {
	next := now.Add(terminalRetention)
	if t.state == terminalRunning && t.stopAt.Before(next) {
		next = t.stopAt
	}
	if t.final != nil && t.final.expiresAt.Before(next) {
		next = t.final.expiresAt
	}
	for _, att := range t.attachments {
		next = earliest(next, att.deadline)
		if att.checkpoint != nil && att.checkpoint.expiresAt.After(now) {
			next = earliest(next, att.checkpoint.expiresAt)
		}
	}
	t.timer.Reset(max(next.Sub(now), 0))
}

func earliest(a, b time.Time) time.Time {
	if b.Before(a) {
		return b
	}
	return a
}
