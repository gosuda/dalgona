//go:build gui

package manage

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"strconv"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/terminal"
)

const (
	readWaitMs          = 1000
	readDeadline        = 5 * time.Second
	readLimit           = 16 << 10
	checkpointReadLimit = 256 << 10
	checkpointMax       = 8 << 20
	inputLimit          = 16 << 10
	unackedInputLimit   = 64
	terminalStateExit   = "exited"
	terminalStateClose  = "closed"
	inputUncertain      = "uncertain"
	inputClosed         = "closed"
)

var (
	errInputBacklog   = fmt.Errorf("%d keystroke writes are still waiting for acknowledgment; wait before typing more", unackedInputLimit)
	errInputUncertain = errors.New("terminal input is uncertain: a write may or may not have reached the shell; stop this terminal or start a new one")
)

type termEnd uint8

const (
	endNone termEnd = iota
	endCollected
	endExpired
	endReleased
	endLost
	endFailed
)

type termSession struct {
	hub        *Hub
	mu         sync.Mutex
	b          binding
	lease      lease
	identity   terminalIdentity
	attachment terminalAttachment
	status     terminalStatus
	replica    *terminal.Replica
	readable   terminal.ReadableOutput
	readOffset uint64
	readResize uint64
	hasReadout bool
	end        termEnd
	endErr     error
	gaps       int
	inputSeq   uint64
	resizeSeq  uint64
	wantCols   int
	wantRows   int
	sentCols   int
	sentRows   int
	resizeOn   bool
	pending    int
	uncertain  bool
	inputErr   error
	queue      chan []byte
	cancel     context.CancelFunc
	wake       func()
	readerDone chan struct{}
	writerDone chan struct{}
	reading    bool
}

func checkpointInfo(c terminalCheckpoint) (terminal.CheckpointInfo, error) {
	next, err := rpc.ParseDecimal(c.NextOffset)
	if err != nil {
		return terminal.CheckpointInfo{}, err
	}
	ordinal, err := rpc.ParseDecimal(c.ResizeOrdinal)
	if err != nil {
		return terminal.CheckpointInfo{}, err
	}
	total, err := rpc.ParseDecimal(c.Total)
	if err != nil || total > checkpointMax {
		return terminal.CheckpointInfo{}, fmt.Errorf("checkpoint size %q exceeds the %d-byte limit", c.Total, checkpointMax)
	}
	return terminal.CheckpointInfo{EngineVersion: c.EngineVersion, Digest: c.Digest, Version: c.Version, NextOffset: next, ResizeOrdinal: ordinal, Columns: c.Columns, Rows: c.Rows, Total: int(total)}, nil
}

func (h *Hub) downloadCheckpoint(b binding, terminalID, attachmentID string, c terminalCheckpoint) (terminal.CheckpointInfo, []byte, error) {
	info, err := checkpointInfo(c)
	if err != nil {
		return info, nil, err
	}
	body := make([]byte, 0, info.Total)
	for len(body) < info.Total {
		var chunk checkpointChunk
		params := checkpointReadParams{TerminalID: terminalID, AttachmentID: attachmentID, CheckpointID: c.CheckpointID, Offset: strconv.Itoa(len(body)), Limit: checkpointReadLimit}
		if err := h.call(b, pageDeadline, methodCheckpointRead, params, &chunk); err != nil {
			return info, nil, err
		}
		data, err := base64.StdEncoding.DecodeString(chunk.DataBase64)
		if err != nil || len(data) == 0 {
			return info, nil, fmt.Errorf("checkpoint chunk at %d is empty or undecodable", len(body))
		}
		body = append(body, data...)
	}
	return info, body, nil
}

func startSession(h *Hub, b binding, l lease, opened terminalOpened, wake func()) (*termSession, error) {
	info, body, err := h.downloadCheckpoint(b, opened.Identity.TerminalID, opened.Attachment.AttachmentID, opened.Checkpoint)
	if err != nil {
		return nil, err
	}
	replica, err := terminal.NewReplica(info, body)
	if err != nil {
		return nil, err
	}
	s := &termSession{hub: h, b: b, lease: l, identity: opened.Identity, attachment: opened.Attachment, status: opened.Status, replica: replica, wake: wake}
	s.inputSeq, _ = rpc.ParseDecimal(cmpOr(opened.Attachment.AcceptedInputSeq, "0"))
	s.resizeSeq, _ = rpc.ParseDecimal(cmpOr(opened.Attachment.AcceptedResizeSeq, "0"))
	s.uncertain = opened.Status.InputState == inputUncertain
	s.run()
	return s, nil
}

func (s *termSession) run() {
	ctx, cancel := context.WithCancel(s.hub.ctx)
	s.cancel, s.queue, s.readerDone, s.writerDone, s.reading = cancel, make(chan []byte, unackedInputLimit), make(chan struct{}), make(chan struct{}), true
	go s.read(ctx)
	go s.write(ctx, s.queue)
}

func (s *termSession) stopWorkers() {
	s.stopReading()
	<-s.readerDone
	<-s.writerDone
}

func (s *termSession) read(ctx context.Context) {
	defer close(s.readerDone)
	defer s.notify()
	for ctx.Err() == nil {
		if s.step(ctx) {
			return
		}
		s.notify()
	}
}

func (s *termSession) notify() {
	if s.wake != nil {
		s.wake()
	}
}

func (s *termSession) cursors() (terminalReadParams, terminalAttachment) {
	s.mu.Lock()
	defer s.mu.Unlock()
	next, ordinal := s.replica.Cursors()
	return terminalReadParams{
		TerminalID: s.identity.TerminalID, AttachmentID: s.attachment.AttachmentID, Offset: rpc.FormatDecimal(next),
		AfterResizeOrdinal: rpc.FormatDecimal(ordinal), Limit: readLimit, WaitMs: readWaitMs,
	}, s.attachment
}

func (s *termSession) step(ctx context.Context) bool {
	params, _ := s.cursors()
	var page terminalPage
	err := s.hub.callContext(ctx, operationRequest{binding: s.b, deadline: readDeadline, method: methodTerminalRead, params: params, result: &page})
	switch kind := kindOf(err); {
	case err == nil:
		return s.apply(page)
	case ctx.Err() != nil:
		return true
	case kind == rpc.ErrorReplayGap:
		return s.recover()
	case kind == rpc.ErrorExpired:
		return s.finish(endExpired, err)
	default:
		return s.finish(endLost, err)
	}
}

func (s *termSession) apply(page terminalPage) bool {
	read, err := decodeRead(page)
	s.mu.Lock()
	if err == nil {
		err = s.replica.Apply(read)
	}
	s.status = page.Status
	if page.Status.InputState == inputUncertain {
		s.uncertain = true
	}
	s.mu.Unlock()
	if err != nil {
		return s.finish(endFailed, err)
	}
	if s.collected() {
		return s.finish(endCollected, nil)
	}
	return s.retentionExpired()
}

func decodeRead(page terminalPage) (terminal.Read, error) {
	offset, err := rpc.ParseDecimal(page.Offset)
	if err != nil {
		return terminal.Read{}, err
	}
	next, err := rpc.ParseDecimal(page.Next)
	if err != nil {
		return terminal.Read{}, err
	}
	ordinal, err := rpc.ParseDecimal(page.ResizeOrdinal)
	if err != nil {
		return terminal.Read{}, err
	}
	data, err := base64.StdEncoding.DecodeString(page.DataBase64)
	if err != nil {
		return terminal.Read{}, err
	}
	resizes := make([]terminal.Resize, 0, len(page.Resizes))
	for _, r := range page.Resizes {
		o, err1 := rpc.ParseDecimal(r.Ordinal)
		at, err2 := rpc.ParseDecimal(r.Offset)
		if err := errors.Join(err1, err2); err != nil {
			return terminal.Read{}, err
		}
		resizes = append(resizes, terminal.Resize{Ordinal: o, Offset: at, Columns: r.Columns, Rows: r.Rows})
	}
	return terminal.Read{Data: data, Resizes: resizes, Offset: offset, Next: next, ResizeOrdinal: ordinal}, nil
}

func (s *termSession) collected() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.status.FinalOffset == "" || s.status.FinalResizeOrdinal == "" {
		return false
	}
	final, err1 := rpc.ParseDecimal(s.status.FinalOffset)
	ordinal, err2 := rpc.ParseDecimal(s.status.FinalResizeOrdinal)
	return err1 == nil && err2 == nil && s.replica.Collected(final, ordinal)
}

func (s *termSession) retentionExpired() bool {
	s.mu.Lock()
	expires := s.status.OutputExpiresAt
	s.mu.Unlock()
	if expires == "" {
		return false
	}
	at, err := time.Parse(time.RFC3339, expires)
	if err != nil || s.hub.deps.Now().Before(at) {
		return false
	}
	return s.finish(endExpired, errors.New("the retained output expired"))
}

func (s *termSession) recover() bool {
	_, att := s.cursors()
	var opened checkpointOpened
	err := s.hub.call(s.b, pageDeadline, methodCheckpointOpen, attachmentRef{TerminalID: s.identity.TerminalID, AttachmentID: att.AttachmentID}, &opened)
	if err != nil {
		return s.finish(endFailed, fmt.Errorf("older output is gone and no fresh checkpoint was available: %w", err))
	}
	info, body, err := s.hub.downloadCheckpoint(s.b, s.identity.TerminalID, att.AttachmentID, opened.Checkpoint)
	s.mu.Lock()
	if err == nil {
		err = s.replica.Refresh(info, body)
	}
	if err == nil {
		s.gaps++
		s.hasReadout = false
		s.status = opened.Status
	}
	s.mu.Unlock()
	if err != nil {
		return s.finish(endFailed, err)
	}
	return false
}

func (s *termSession) finish(end termEnd, err error) bool {
	s.mu.Lock()
	s.end, s.endErr, s.reading = end, err, false
	s.mu.Unlock()
	s.stopReading()
	if end == endExpired {
		s.hub.workspaces.release(s.lease)
	}
	return true
}

func (s *termSession) send(data []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	chunks := (len(data) + inputLimit - 1) / inputLimit
	switch {
	case s.uncertain:
		return errInputUncertain
	case s.attachment.WriterLease == "" || s.status.InputState == inputClosed || s.end != endNone:
		return errors.New("this attachment cannot type into the terminal")
	case s.pending+chunks > unackedInputLimit:
		return errInputBacklog
	}
	for len(data) > 0 {
		n := min(len(data), inputLimit)
		s.pending++
		s.queue <- data[:n:n]
		data = data[n:]
	}
	return nil
}

func (s *termSession) write(ctx context.Context, queue chan []byte) {
	defer close(s.writerDone)
	for {
		select {
		case <-ctx.Done():
			return
		case data := <-queue:
			s.writeOne(ctx, data)
		}
	}
}

func (s *termSession) writeOne(ctx context.Context, data []byte) {
	s.mu.Lock()
	skip := s.uncertain
	s.inputSeq++
	params := terminalWriteParams{TerminalID: s.identity.TerminalID, WriterLease: s.attachment.WriterLease, InputSeq: rpc.FormatDecimal(s.inputSeq), DataBase64: base64.StdEncoding.EncodeToString(data)}
	s.mu.Unlock()
	var err error
	if !skip {
		var ack terminalAccepted
		err = s.hub.callContext(ctx, operationRequest{binding: s.b, deadline: pageDeadline, method: methodTerminalWrite, params: params, result: &ack})
	}
	s.mu.Lock()
	s.pending--
	if err != nil && !skip && ctx.Err() == nil {
		s.uncertain, s.inputErr = s.uncertain || uncertain(err) || kindOf(err) == rpc.ErrorUnavailable, err
	}
	s.mu.Unlock()
	s.notify()
}

func (s *termSession) resize(columns, rows int) {
	s.mu.Lock()
	if s.uncertain || s.attachment.WriterLease == "" || s.end != endNone {
		s.mu.Unlock()
		return
	}
	s.wantCols, s.wantRows = columns, rows
	if s.resizeOn {
		s.mu.Unlock()
		return
	}
	s.resizeOn = true
	s.mu.Unlock()
	go s.resizeLoop()
}

func (s *termSession) resizeLoop() {
	for {
		s.mu.Lock()
		if s.wantCols == s.sentCols && s.wantRows == s.sentRows {
			s.resizeOn = false
			s.mu.Unlock()
			return
		}
		s.resizeSeq++
		params := terminalResizeParams{TerminalID: s.identity.TerminalID, WriterLease: s.attachment.WriterLease, ResizeSeq: rpc.FormatDecimal(s.resizeSeq), Columns: s.wantCols, Rows: s.wantRows}
		wantCols, wantRows := s.wantCols, s.wantRows
		s.mu.Unlock()
		var ack terminalAccepted
		err := s.hub.call(s.b, pageDeadline, methodTerminalResize, params, &ack)
		s.mu.Lock()
		switch {
		case uncertain(err):
			s.uncertain, s.inputErr, s.resizeOn = true, err, false
			s.mu.Unlock()
			s.notify()
			return
		case kindOf(err) == rpc.ErrorOperationConflict:
			s.mu.Unlock()
			continue
		case err != nil:
			s.sentCols, s.sentRows, s.resizeOn = wantCols, wantRows, false
			s.mu.Unlock()
			return
		default:
			s.sentCols, s.sentRows = wantCols, wantRows
			s.mu.Unlock()
		}
	}
}

func (s *termSession) stopReading() {
	if s.cancel != nil {
		s.cancel()
	}
}

func (s *termSession) detach() error {
	s.stopReading()
	<-s.readerDone
	<-s.writerDone
	var res terminalDetached
	_, att := s.cursors()
	err := s.hub.call(s.b, metadataDeadline, methodTerminalDetach, attachmentRef{TerminalID: s.identity.TerminalID, AttachmentID: att.AttachmentID}, &res)
	s.mu.Lock()
	s.end = endReleased
	s.mu.Unlock()
	s.hub.workspaces.release(s.lease)
	return err
}

func (s *termSession) offsets() (uint64, uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.replica.Cursors()
}

func (s *termSession) history() (int64, int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.replica.Lines()
}

func (s *termSession) search(query string) terminal.SearchResult {
	if query == "" {
		first, end := s.history()
		return terminal.SearchResult{FirstLine: first, EndLine: end}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.replica.Search(query, false)
}

type termSnapshot struct {
	status    terminalStatus
	end       termEnd
	endErr    error
	gaps      int
	uncertain bool
	inputErr  error
	writer    bool
	reading   bool
	pending   int
	identity  terminalIdentity
	dropped   int
	columns   int
	rows      int
}

func (s *termSession) snapshot() termSnapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	columns, rows := s.replica.Size()
	return termSnapshot{
		status: s.status, end: s.end, endErr: s.endErr, gaps: s.gaps, uncertain: s.uncertain, inputErr: s.inputErr,
		writer: s.attachment.WriterLease != "", reading: s.reading, pending: s.pending, identity: s.identity,
		dropped: s.replica.DroppedScrollbackRows(), columns: columns, rows: rows,
	}
}

func (s *termSession) window(v *terminal.View, top int64, height int, follow bool, sel *terminal.Selection) int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	first, end := s.replica.Lines()
	if follow {
		top = end - int64(height)
	}
	top = max(first, min(top, end-int64(height)))
	s.replica.Window(v, top, height)
	v.HasSelection = sel != nil
	if sel != nil {
		v.Selection = s.replica.Extent(*sel)
	}
	return top
}

func (s *termSession) readout() terminal.ReadableOutput {
	s.mu.Lock()
	defer s.mu.Unlock()
	offset, resize := s.replica.Cursors()
	if !s.hasReadout || s.readOffset != offset || s.readResize != resize {
		s.readable = s.replica.ReadableOutput()
		s.readOffset, s.readResize, s.hasReadout = offset, resize, true
	}
	return s.readable
}

func (s *termSession) selected(sel terminal.Selection) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.replica.Text(s.replica.Extent(sel))
}

func (s *termSession) encodeKey(k terminal.Key, mods terminal.Modifiers) []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return terminal.EncodeKey(nil, k, mods, s.replica.InputModes())
}

func (s *termSession) encodePaste(text string) []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return terminal.EncodePaste(nil, text, s.replica.InputModes())
}

func (s *termSession) encodeFocus(focused bool) []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return terminal.EncodeFocus(nil, focused, s.replica.InputModes())
}

func (s *termSession) encodeMouse(ev terminal.MouseEvent) []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return terminal.EncodeMouse(nil, ev, s.replica.InputModes())
}

func (s *termSession) screenTop() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.replica.ScreenTop()
}

func (s *termSession) lineEnd() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, end := s.replica.Lines()
	return end
}

func (s *termSession) reportsMouse() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	m := s.replica.InputModes()
	return m.MouseX10 || m.MouseNormal || m.MouseButton || m.MouseAny
}
