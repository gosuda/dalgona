package rpcserver

import (
	"context"
	"crypto/sha256"
	"encoding/json/jsontext"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func terminalMethods() map[string]method {
	return map[string]method{
		"dalgona/terminal/open":            deadline(PageDeadline, withOperation("dalgona/terminal/open", (*server).terminalOpen)),
		"dalgona/terminal/list":            deadline(MetadataDeadline, (*server).terminalList),
		"dalgona/terminal/attach":          deadline(PageDeadline, (*server).terminalAttach),
		"dalgona/terminal/checkpoint/open": deadline(PageDeadline, (*server).terminalCheckpointOpen),
		"dalgona/terminal/checkpoint/read": deadline(PageDeadline, (*server).terminalCheckpointRead),
		"dalgona/terminal/read":            deadline(PageDeadline, (*server).terminalRead),
		"dalgona/terminal/write":           deadline(PageDeadline, (*server).terminalWrite),
		"dalgona/terminal/resize":          deadline(PageDeadline, (*server).terminalResize),
		"dalgona/terminal/close":           deadline(PageDeadline, withOperation("dalgona/terminal/close", (*server).terminalClose)),
		"dalgona/terminal/detach":          deadline(MetadataDeadline, (*server).terminalDetach),
	}
}

func (s *server) terminalHost() (*TerminalHost, error) {
	if s.opts.Terminals == nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if s.native.Load() == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	return s.opts.Terminals, nil
}

func (s *server) terminalOpen(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	host, err := s.terminalHost()
	if err != nil {
		return nil, err
	}
	p, err := decodeTerminalParams(params, "workspaceId", "columns", "rows")
	if err != nil {
		return nil, err
	}
	columns, rows, err := p.size()
	if err != nil {
		return nil, err
	}
	grant, release, err := s.workspaceTerminal(p.workspaceID)
	if err != nil {
		return nil, err
	}
	t, att, err := s.launchTerminal(ctx, host, grant, terminalLaunch{workspaceID: p.workspaceID, release: release, columns: columns, rows: rows})
	if err != nil {
		release()
		return nil, err
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	return encodeAttached(t, att)
}

type terminalLaunch struct {
	workspaceID string
	release     func()
	columns     int
	rows        int
}

func (s *server) launchTerminal(ctx context.Context, host *TerminalHost, grant workspaceGrant, launch terminalLaunch) (*terminal, *attachment, error) {
	opener, ok := grant.run.(runner.TerminalOpener)
	if !ok {
		return nil, nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if grant.handle != nil {
		if err := verifyPinned(ctx, grant.handle); err != nil {
			return nil, nil, err
		}
	}
	s.terminalMu.Lock()
	defer s.terminalMu.Unlock()
	if host.connectionTerminals(s) >= connectionTerminals {
		return nil, nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	if err := host.reserve(); err != nil {
		return nil, nil, err
	}
	spec := runner.TerminalSpec{Dir: grant.root, Sandbox: terminalSandbox(s.host.Config(), grant.root), Columns: launch.columns, Rows: launch.rows}
	proc, err := opener.OpenTerminal(ctx, spec)
	if err != nil {
		host.unreserve()
		s.log.Warn("rpc: open terminal", "runner", grant.runnerID, "error", err)
		return nil, nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	identity := terminalIdentity{threadID: grant.threadID, runnerID: grant.runnerID, bindingRevision: grant.bindingRevision, root: grant.root}
	t := newTerminal(host, identity, proc, launch.columns, launch.rows)
	t.mu.Lock()
	att, _, err := t.attachLocked(attachRequest{owner: s, workspaceID: launch.workspaceID, release: launch.release, validate: grant.validate, access: accessWrite}, time.Now())
	t.mu.Unlock()
	if err != nil {
		host.unreserve()
		t.timer.Stop()
		_ = proc.Signal(runner.SignalKill)
		_ = proc.Close()
		return nil, nil, err
	}
	host.insert(t)
	t.start()
	return t, att, nil
}

func (s *server) terminalList(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	host, err := s.terminalHost()
	if err != nil {
		return nil, err
	}
	p, err := decodeTerminalParams(params, "workspaceId")
	if err != nil {
		return nil, err
	}
	grant, release, err := s.workspaceTerminal(p.workspaceID)
	if err != nil {
		return nil, err
	}
	release()
	identity := terminalIdentity{threadID: grant.threadID, runnerID: grant.runnerID, bindingRevision: grant.bindingRevision, root: grant.root}
	listings := terminalListings{}
	for _, t := range host.all() {
		if entry, ok := t.listing(identity); ok {
			listings = append(listings, entry)
		}
	}
	slices.SortFunc(listings, func(a, b terminalListing) int { return strings.Compare(a.id, b.id) })
	return rpc.Marshal(listings[:min(len(listings), terminalListLimit)])
}

func (t *terminal) listing(identity terminalIdentity) (terminalListing, bool) {
	if t.identity != identity {
		return terminalListing{}, false
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.released {
		return terminalListing{}, false
	}
	return terminalListing{
		id: t.id, identity: t.identity, status: t.statusLocked(), writerPresent: t.writer != nil,
		lease: rpc.FormatTimestamp(t.leaseDeadline(t.stopAt)),
	}, true
}

func (s *server) terminalAttach(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	host, err := s.terminalHost()
	if err != nil {
		return nil, err
	}
	p, err := decodeTerminalParams(params, "workspaceId", "terminalId", "access")
	if err != nil || (p.access != accessRead && p.access != accessWrite) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	grant, release, err := s.workspaceTerminal(p.workspaceID)
	if err != nil {
		return nil, err
	}
	req := attachRequest{owner: s, workspaceID: p.workspaceID, release: release, validate: grant.validate, access: p.access, resumeLease: p.resumeLease}
	result, err := s.attachTerminal(host, grant, req, p.terminalID)
	if err != nil {
		release()
	}
	return result, err
}

func (s *server) attachTerminal(host *TerminalHost, grant workspaceGrant, req attachRequest, terminalID string) (jsontext.Value, error) {
	t, err := host.lookup(terminalID)
	if err != nil {
		return nil, err
	}
	if t.identity != (terminalIdentity{threadID: grant.threadID, runnerID: grant.runnerID, bindingRevision: grant.bindingRevision, root: grant.root}) {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	s.terminalMu.Lock()
	defer s.terminalMu.Unlock()
	if !t.attachedBy(s) && host.connectionTerminals(s) >= connectionTerminals {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	t.mu.Lock()
	att, resumed, err := t.attachLocked(req, time.Now())
	if err != nil {
		t.mu.Unlock()
		return nil, err
	}
	result, err := encodeAttached(t, att)
	t.mu.Unlock()
	if resumed != nil {
		releaseWorkspaces([]*attachment{resumed})
	}
	return result, err
}

func (s *server) terminalSession(params jsontext.Value, required ...string) (*terminal, terminalParams, error) {
	host, err := s.terminalHost()
	if err != nil {
		return nil, terminalParams{}, err
	}
	p, err := decodeTerminalParams(params, required...)
	if err != nil {
		return nil, terminalParams{}, err
	}
	t, err := host.lookup(p.terminalID)
	return t, p, err
}

func (s *server) terminalCheckpointOpen(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, p, err := s.terminalSession(params, "terminalId", "attachmentId")
	if err != nil {
		return nil, err
	}
	att, err := t.lockAttachment(s, p.attachmentID)
	if err != nil {
		return nil, err
	}
	defer t.mu.Unlock()
	now := time.Now()
	cp, err := t.refreshLocked(att, now)
	if err != nil {
		return nil, err
	}
	return encodeRefreshed(t, cp)
}

func (s *server) terminalCheckpointRead(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, p, err := s.terminalSession(params, "terminalId", "attachmentId", "checkpointId")
	if err != nil {
		return nil, err
	}
	offset, err := p.checkpointOffset()
	if err != nil {
		return nil, err
	}
	limit, err := p.bounded("limit", p.limit, numberRange{fallback: checkpointReadDefault, least: 1, most: checkpointReadMax})
	if err != nil {
		return nil, err
	}
	att, err := t.lockAttachment(s, p.attachmentID)
	if err != nil {
		return nil, err
	}
	cp, err := t.openCheckpointRead(att, p.checkpointID, time.Now())
	t.mu.Unlock()
	if err != nil {
		return nil, err
	}
	defer t.closeCheckpointRead(cp)
	if offset > int64(len(cp.body)) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	chunk, err := contentChunk(cp.body, cp.digest, offset, int64(limit))
	if err != nil {
		return nil, err
	}
	return marshalChunk(chunk)
}

func (s *server) terminalRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, p, err := s.terminalSession(params, "terminalId", "attachmentId", "offset", "afterResizeOrdinal")
	if err != nil {
		return nil, err
	}
	cursor, err := p.cursor()
	if err != nil {
		return nil, err
	}
	limit, err := p.bounded("limit", p.limit, numberRange{fallback: terminalReadDefault, least: 1, most: terminalReadMax})
	if err != nil {
		return nil, err
	}
	wait, err := p.bounded("waitMs", p.waitMs, numberRange{least: 0, most: terminalWaitMax})
	if err != nil {
		return nil, err
	}
	until := time.Now().Add(time.Duration(wait) * time.Millisecond)
	for {
		result, changed, err := t.readPage(s, p.attachmentID, cursor, limit, until)
		if changed == nil {
			return result, err
		}
		if !waitForTerminal(ctx, changed, until) {
			return nil, ctx.Err()
		}
	}
}

func (t *terminal) readPage(s *server, attachmentID string, cursor replayCursor, limit int, until time.Time) (jsontext.Value, <-chan struct{}, error) {
	att, err := t.lockAttachment(s, attachmentID)
	if err != nil {
		return nil, nil, err
	}
	defer t.mu.Unlock()
	now := time.Now()
	if validateErr := t.resizes.validate(cursor, &t.output); validateErr != nil {
		return nil, nil, validateErr
	}
	page, err := assemblePage(&t.output, &t.resizes, cursor, limit)
	if err != nil {
		return nil, nil, err
	}
	if page.empty() && now.Before(until) && t.final == nil {
		return nil, t.changed, nil
	}
	t.renewLocked(att, now)
	result, err := encodePage(t, page, att)
	return result, nil, err
}

func waitForTerminal(ctx context.Context, changed <-chan struct{}, until time.Time) bool {
	timer := time.NewTimer(time.Until(until))
	defer timer.Stop()
	select {
	case <-changed:
	case <-timer.C:
	case <-ctx.Done():
		return false
	}
	return true
}

func (s *server) terminalWrite(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, p, err := s.terminalSession(params, "terminalId", "writerLease", "inputSeq", "dataBase64")
	if err != nil {
		return nil, err
	}
	seq, err := p.sequence(p.inputSeq)
	if err != nil {
		return nil, err
	}
	data, err := p.input()
	if err != nil {
		return nil, err
	}
	input := terminalInput{owner: s, lease: p.writerLease, seq: seq, data: data, digest: sha256.Sum256(data)}
	if acked, err := t.inputRepeated(input); acked || err != nil {
		return encodeSeqOrError(seq, err)
	}
	return t.dispatchInput(input)
}

type terminalInput struct {
	owner  *server
	lease  string
	seq    uint64
	data   []byte
	digest [sha256.Size]byte
}

func encodeSeqOrError(seq uint64, err error) (jsontext.Value, error) {
	if err != nil {
		return nil, err
	}
	return encodeAcceptedSeq(seq)
}

func (t *terminal) inputRepeated(input terminalInput) (bool, error) {
	lease, err := t.lockWriter(input.owner, input.lease)
	if err != nil {
		return false, err
	}
	defer t.mu.Unlock()
	_, repeated, err := lease.input.repeated(input.seq, input.digest)
	return repeated, err
}

func (t *terminal) dispatchInput(input terminalInput) (jsontext.Value, error) {
	t.input.Lock()
	defer t.input.Unlock()
	lease, err := t.reserveInput(input)
	if lease == nil {
		return encodeSeqOrError(input.seq, err)
	}
	written, writeErr := t.proc.Write(input.data)
	t.mu.Lock()
	defer t.mu.Unlock()
	if writeErr != nil || written != len(input.data) {
		lease.input.settle(input.seq, sequenceUnknown, terminalResize{})
		if t.inputState == inputReady {
			t.inputState = inputUncertain
		}
		t.notifyLocked()
		return nil, rpc.NativeKind(rpc.ErrorUnknownOutcome)
	}
	lease.input.settle(input.seq, sequenceDone, terminalResize{})
	return encodeAcceptedSeq(input.seq)
}

func (t *terminal) reserveInput(input terminalInput) (*writerLease, error) {
	lease, err := t.lockWriter(input.owner, input.lease)
	if err != nil {
		return nil, err
	}
	defer t.mu.Unlock()
	if _, repeated, err := lease.input.repeated(input.seq, input.digest); repeated || err != nil {
		return nil, err
	}
	if t.inputState != inputReady {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	lease.input.reserve(input.seq, input.digest)
	t.renewLocked(lease.holder, time.Now())
	return lease, nil
}

func (s *server) terminalResize(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, p, err := s.terminalSession(params, "terminalId", "writerLease", "resizeSeq", "columns", "rows")
	if err != nil {
		return nil, err
	}
	seq, err := p.sequence(p.resizeSeq)
	if err != nil {
		return nil, err
	}
	columns, rows, err := p.size()
	if err != nil {
		return nil, err
	}
	digest := sha256.Sum256([]byte(strconv.Itoa(columns) + "x" + strconv.Itoa(rows)))
	lease, err := t.lockWriter(s, p.writerLease)
	if err != nil {
		return nil, err
	}
	defer t.mu.Unlock()
	record, repeated, err := lease.resize.repeated(seq, digest)
	if repeated || err != nil {
		return encodeResizedOrError(seq, record.resize, err)
	}
	if t.inputState != inputReady {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	lease.resize.reserve(seq, digest)
	return t.commitResizeLocked(lease, seq, columns, rows)
}

func encodeResizedOrError(seq uint64, record terminalResize, err error) (jsontext.Value, error) {
	if err != nil {
		return nil, err
	}
	return encodeResized(seq, record)
}

func (t *terminal) commitResizeLocked(lease *writerLease, seq uint64, columns, rows int) (jsontext.Value, error) {
	if err := t.proc.Resize(columns, rows); err != nil {
		lease.resize.settle(seq, sequenceUnknown, terminalResize{})
		t.inputState = inputUncertain
		t.notifyLocked()
		return nil, rpc.NativeKind(rpc.ErrorUnknownOutcome)
	}
	t.screen.Resize(columns, rows)
	t.columns, t.rows = columns, rows
	record := t.resizes.append(t.output.next(), columns, rows)
	lease.resize.settle(seq, sequenceDone, record)
	t.renewLocked(lease.holder, time.Now())
	t.notifyLocked()
	return encodeResized(seq, record)
}

func (s *server) terminalClose(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	t, _, err := s.terminalSession(params, "terminalId")
	if err != nil {
		return nil, err
	}
	if !t.authorizedBy(s) {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	t.requestClose()
	select {
	case <-t.stopped:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	t.mu.Lock()
	unconfirmed := t.unconfirmed
	t.mu.Unlock()
	if unconfirmed {
		return nil, rpc.NativeKind(rpc.ErrorUnknownOutcome)
	}
	select {
	case <-t.finished:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	return encodeTerminalStatus(t.statusLocked())
}

func (t *terminal) authorizedBy(s *server) bool {
	t.mu.Lock()
	var ids [terminalAttachmentLimit]string
	count := 0
	for id, att := range t.attachments {
		if att.owner == s {
			ids[count] = id
			count++
		}
	}
	t.mu.Unlock()
	for _, id := range ids[:count] {
		if _, err := t.lockAttachment(s, id); err == nil {
			t.mu.Unlock()
			return true
		}
	}
	return false
}

func (s *server) terminalDetach(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	host, err := s.terminalHost()
	if err != nil {
		return nil, err
	}
	p, err := decodeTerminalParams(params, "terminalId", "attachmentId")
	if err != nil {
		return nil, err
	}
	t, err := host.lookup(p.terminalID)
	if err != nil {
		return encodeDetached("notFound")
	}
	return encodeDetached(t.detach(s, p.attachmentID))
}
