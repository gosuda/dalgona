//go:build unix

package rpcserver

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type terminalCheckpointWire struct {
	CheckpointID  string `json:"checkpointId"`
	Version       int    `json:"version"`
	EngineVersion string `json:"engineVersion"`
	NextOffset    string `json:"nextOffset"`
	ResizeOrdinal string `json:"resizeOrdinal"`
	Columns       int    `json:"columns"`
	Rows          int    `json:"rows"`
	Total         string `json:"total"`
	Digest        string `json:"digest"`
}

type terminalAttachedWire struct {
	Identity struct {
		TerminalID       string `json:"terminalId"`
		DaemonInstanceID string `json:"daemonInstanceId"`
		ThreadID         string `json:"threadId"`
	} `json:"identity"`
	Attachment struct {
		AttachmentID     string `json:"attachmentId"`
		WriterLease      string `json:"writerLease"`
		AcceptedInputSeq string `json:"acceptedInputSeq"`
	} `json:"attachment"`
	Checkpoint terminalCheckpointWire `json:"checkpoint"`
	Status     terminalStatusWire     `json:"status"`
}

type terminalRig struct {
	t           *testing.T
	r           *rig
	host        *TerminalHost
	c           *nativeClient
	threadID    string
	workspaceID string
}

func newTerminalRig(t *testing.T, script string) *terminalRig {
	t.Helper()
	shell := filepath.Join(t.TempDir(), "shell.sh")
	if err := os.WriteFile(shell, []byte("#!/bin/sh\n"+script), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("SHELL", shell)
	r := newRig(t, config.ApprovalAuto)
	host := NewTerminalHost(transcriptEngine)
	c := r.connectNative(ServeOptions{Terminals: host})
	threadID := c.startThread(r)
	tr := &terminalRig{t: t, r: r, host: host, c: c, threadID: threadID}
	tr.workspaceID = tr.openWorkspace(c)
	t.Cleanup(func() {
		for _, term := range host.all() {
			term.requestClose()
			<-term.stopped
		}
	})
	return tr
}

func (tr *terminalRig) openWorkspace(c *nativeClient) string {
	tr.t.Helper()
	go func() {
		select {
		case request := <-c.requests:
			c.send(message{ID: request.ID, Result: jsontext.Value(`{"decision":"allowOnce"}`)})
		case <-time.After(waitLimit):
		}
	}()
	var opened struct {
		WorkspaceID string `json:"workspaceId"`
	}
	params := map[string]any{"threadId": tr.threadID, "root": "cwd", "rights": []string{"terminal"}, "operationId": rpc.NewOperationID()}
	c.ok("dalgona/workspace/open", params, &opened)
	return opened.WorkspaceID
}

func (tr *terminalRig) open(columns, rows int) terminalAttachedWire {
	tr.t.Helper()
	var opened terminalAttachedWire
	params := map[string]any{"workspaceId": tr.workspaceID, "columns": columns, "rows": rows, "operationId": rpc.NewOperationID()}
	tr.c.ok("dalgona/terminal/open", params, &opened)
	return opened
}

func (tr *terminalRig) attach(c *nativeClient, workspaceID, terminalID, access string) terminalAttachedWire {
	tr.t.Helper()
	var attached terminalAttachedWire
	c.ok("dalgona/terminal/attach", map[string]any{"workspaceId": workspaceID, "terminalId": terminalID, "access": access}, &attached)
	return attached
}

type terminalReader struct {
	tr           *terminalRig
	c            *nativeClient
	terminalID   string
	attachmentID string
	offset       string
	ordinal      string
	data         []byte
	resizes      []terminalResizeWire
	status       terminalStatusWire
}

func (tr *terminalRig) reader(c *nativeClient, terminalID, attachmentID, offset, ordinal string) *terminalReader {
	return &terminalReader{tr: tr, c: c, terminalID: terminalID, attachmentID: attachmentID, offset: offset, ordinal: ordinal}
}

func (rd *terminalReader) page(limit int) terminalPageWire {
	rd.tr.t.Helper()
	var page terminalPageWire
	params := map[string]any{
		"terminalId": rd.terminalID, "attachmentId": rd.attachmentID, "offset": rd.offset, "afterResizeOrdinal": rd.ordinal,
		"limit": limit, "waitMs": 200,
	}
	rd.c.ok("dalgona/terminal/read", params, &page)
	data, err := base64.StdEncoding.DecodeString(page.DataBase64)
	if err != nil {
		rd.tr.t.Fatal(err)
	}
	rd.data = append(rd.data, data...)
	rd.resizes = append(rd.resizes, page.Resizes...)
	rd.offset, rd.ordinal, rd.status = page.Next, page.ResizeOrdinal, page.Status
	return page
}

func (rd *terminalReader) until(done func(*terminalReader) bool) {
	rd.tr.t.Helper()
	deadline := time.Now().Add(waitLimit)
	for !done(rd) {
		if time.Now().After(deadline) {
			rd.tr.t.Fatalf("terminal output never satisfied the reader; saw %q at %s/%s status %+v", rd.data, rd.offset, rd.ordinal, rd.status)
		}
		rd.page(terminalReadDefault)
	}
}

func containing(want string) func(*terminalReader) bool {
	return func(rd *terminalReader) bool { return bytes.Contains(rd.data, []byte(want)) }
}

func (tr *terminalRig) writeTo(terminalID, lease string, seq int, data string) {
	tr.t.Helper()
	params := map[string]any{"terminalId": terminalID, "writerLease": lease, "inputSeq": strconv.Itoa(seq), "dataBase64": base64.StdEncoding.EncodeToString([]byte(data))}
	tr.c.ok("dalgona/terminal/write", params, nil)
}

func (tr *terminalRig) downloadCheckpoint(c *nativeClient, terminalID, attachmentID string, cp terminalCheckpointWire, limit int) []byte {
	tr.t.Helper()
	var body []byte
	for offset := "0"; ; {
		var chunk rpc.ContentChunk
		params := map[string]any{"terminalId": terminalID, "attachmentId": attachmentID, "checkpointId": cp.CheckpointID, "offset": offset, "limit": limit}
		c.ok("dalgona/terminal/checkpoint/read", params, &chunk)
		data, err := base64.StdEncoding.DecodeString(chunk.DataBase64)
		if err != nil {
			tr.t.Fatal(err)
		}
		body = append(body, data...)
		if chunk.Next == chunk.Total {
			break
		}
		offset = chunk.Next
	}
	sum := sha256.Sum256(body)
	if hex.EncodeToString(sum[:]) != cp.Digest || strconv.Itoa(len(body)) != cp.Total {
		tr.t.Fatalf("checkpoint %s reassembled %d bytes digest %x; it declared %s bytes digest %s", cp.CheckpointID, len(body), sum, cp.Total, cp.Digest)
	}
	return body
}

func TestTerminalResizesAtOneOffsetReplayInOrderToEveryAttachment(t *testing.T) {
	tr := newTerminalRig(t, "printf ready\nread line\nprintf 'after:%s' \"$line\"\nread line\n")
	opened := tr.open(80, 24)
	id, lease := opened.Identity.TerminalID, opened.Attachment.WriterLease
	writer := tr.reader(tr.c, id, opened.Attachment.AttachmentID, opened.Checkpoint.NextOffset, opened.Checkpoint.ResizeOrdinal)
	writer.until(containing("ready"))
	viewer := tr.attach(tr.c, tr.workspaceID, id, accessRead)
	const resizes = terminalReadResizes + 2
	applyResizes(t, tr, id, lease, resizes)
	tr.writeTo(id, lease, 1, "go\r")
	readerOfViewer := tr.reader(tr.c, id, viewer.Attachment.AttachmentID, viewer.Checkpoint.NextOffset, viewer.Checkpoint.ResizeOrdinal)
	checkFirstResizePage(t, readerOfViewer)
	readerOfViewer.until(containing("after:go"))
	writer.until(containing("after:go"))
	for _, rd := range []*terminalReader{writer, readerOfViewer} {
		checkResizeRecords(t, rd, resizes)
	}
	if viewer.Checkpoint.NextOffset != "5" || !bytes.Equal(writer.data[5:], readerOfViewer.data) {
		t.Fatalf("viewer joined at byte %s and replayed %q; the writer saw %q", viewer.Checkpoint.NextOffset, readerOfViewer.data, writer.data)
	}
}

func applyResizes(t *testing.T, tr *terminalRig, id, lease string, resizes int) {
	t.Helper()
	for seq := 1; seq <= resizes; seq++ {
		var acked struct {
			Ordinal string `json:"ordinal"`
			Offset  string `json:"offset"`
		}
		params := map[string]any{"terminalId": id, "writerLease": lease, "resizeSeq": strconv.Itoa(seq), "columns": 20 + seq, "rows": 24}
		tr.c.ok("dalgona/terminal/resize", params, &acked)
		if acked.Ordinal != strconv.Itoa(seq) || acked.Offset != "5" {
			t.Fatalf("resize %d acknowledged ordinal %s offset %s, want ordinal %d at offset 5", seq, acked.Ordinal, acked.Offset, seq)
		}
	}
}

func checkFirstResizePage(t *testing.T, readerOfViewer *terminalReader) {
	t.Helper()
	first := readerOfViewer.page(terminalReadMax)
	if len(first.Resizes) != terminalReadResizes || first.Next != "5" || first.ResizeOrdinal != strconv.Itoa(terminalReadResizes) {
		t.Fatalf("first page delivered %d resizes, next %s, ordinal %s; want %d resizes stopping before byte 5", len(first.Resizes), first.Next, first.ResizeOrdinal, terminalReadResizes)
	}
}

func checkResizeRecords(t *testing.T, rd *terminalReader, resizes int) {
	t.Helper()
	if len(rd.resizes) != resizes {
		t.Fatalf("reader received %d resize records, want %d", len(rd.resizes), resizes)
	}
	for i, record := range rd.resizes {
		if record.Ordinal != strconv.Itoa(i+1) || record.Offset != "5" || record.Columns != 21+i {
			t.Fatalf("resize record %d = %+v, want ordinal %d at offset 5 with %d columns", i, record, i+1, 21+i)
		}
	}
}

func TestTerminalCheckpointAtSplitUTF8ReconstructsTheStream(t *testing.T) {
	tr := newTerminalRig(t, "printf '\\344\\270'\nread line\nprintf '\\255 done\\n'\nread line\n")
	opened := tr.open(80, 24)
	id, att := opened.Identity.TerminalID, opened.Attachment.AttachmentID
	full := tr.reader(tr.c, id, att, "0", "0")
	full.until(func(rd *terminalReader) bool { return rd.offset == "2" })
	var refreshed struct {
		Checkpoint terminalCheckpointWire `json:"checkpoint"`
	}
	tr.c.ok("dalgona/terminal/checkpoint/open", map[string]any{"terminalId": id, "attachmentId": att}, &refreshed)
	cp := refreshed.Checkpoint
	if cp.NextOffset != "2" || cp.CheckpointID == opened.Checkpoint.CheckpointID {
		t.Fatalf("refreshed checkpoint %+v, want a new checkpoint at byte 2", cp)
	}
	var state transcriptState
	if err := json.Unmarshal(tr.downloadCheckpoint(tr.c, id, att, cp, 7), &state); err != nil {
		t.Fatal(err)
	}
	tr.writeTo(id, opened.Attachment.WriterLease, 1, "x\r")
	rest := tr.reader(tr.c, id, att, cp.NextOffset, cp.ResizeOrdinal)
	rest.until(containing("done"))
	full.until(containing("done"))
	rebuilt := append(slices.Clone(state.Tail), rest.data...)
	if state.Consumed != 2 || !bytes.Equal(rebuilt, full.data) {
		t.Fatalf("checkpoint state %q (consumed %d) plus %q differs from the stream %q", state.Tail, state.Consumed, rest.data, full.data)
	}
	if !bytes.HasPrefix(full.data, []byte("\xe4\xb8x")) {
		t.Fatalf("stream %q does not keep the split UTF-8 prefix before the echoed input", full.data)
	}
}

func TestTerminalRefusedRefreshKeepsThePreviousCheckpoint(t *testing.T) {
	tr := newTerminalRig(t, "read line\nprintf HUGE\nread line\n")
	opened := tr.open(80, 24)
	id, att := opened.Identity.TerminalID, opened.Attachment.AttachmentID
	tr.writeTo(id, opened.Attachment.WriterLease, 1, "go\r")
	tr.reader(tr.c, id, att, "0", "0").until(containing("HUGE"))
	_ = tr.c.failsWith("dalgona/terminal/checkpoint/open", map[string]any{"terminalId": id, "attachmentId": att}, rpc.ErrorLimitExceeded)
	tr.downloadCheckpoint(tr.c, id, att, opened.Checkpoint, checkpointReadMax)
	tr.reader(tr.c, id, att, "0", "0").until(containing("HUGE"))
}

func TestTerminalFourOccupiedSlotsRecoverThroughCheckpointRefresh(t *testing.T) {
	tr := newTerminalRig(t, "read line\nhead -c 9000000 /dev/zero | tr '\\000' a\nprintf END\nread line\n")
	opened := tr.open(80, 24)
	id := opened.Identity.TerminalID
	readers := []terminalAttachedWire{opened}
	for range terminalAttachmentLimit - 1 {
		readers = append(readers, tr.attach(tr.c, tr.workspaceID, id, accessRead))
	}
	_ = tr.c.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": tr.workspaceID, "terminalId": id, "access": accessRead}, rpc.ErrorLimitExceeded)
	tr.writeTo(id, opened.Attachment.WriterLease, 1, "go\r")
	last := readers[len(readers)-1]
	att := last.Attachment.AttachmentID
	deadline := time.Now().Add(waitLimit)
	for {
		m := tr.c.call("dalgona/terminal/read", map[string]any{"terminalId": id, "attachmentId": att, "offset": "0", "afterResizeOrdinal": "0", "limit": 1})
		if m.Error != nil && m.Error.Code == -32018 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("byte ring never evicted the first byte")
		}
		time.Sleep(50 * time.Millisecond)
	}
	var refreshed struct {
		Checkpoint terminalCheckpointWire `json:"checkpoint"`
	}
	tr.c.ok("dalgona/terminal/checkpoint/open", map[string]any{"terminalId": id, "attachmentId": att}, &refreshed)
	_ = tr.c.failsWith("dalgona/terminal/checkpoint/read", map[string]any{"terminalId": id, "attachmentId": att, "checkpointId": last.Checkpoint.CheckpointID}, rpc.ErrorExpired)
	var state transcriptState
	if err := json.Unmarshal(tr.downloadCheckpoint(tr.c, id, att, refreshed.Checkpoint, checkpointReadMax), &state); err != nil {
		t.Fatal(err)
	}
	if strconv.Itoa(state.Consumed) != refreshed.Checkpoint.NextOffset {
		t.Fatalf("checkpoint state consumed %d bytes but resumes reads at %s", state.Consumed, refreshed.Checkpoint.NextOffset)
	}
	recovered := tr.reader(tr.c, id, att, refreshed.Checkpoint.NextOffset, refreshed.Checkpoint.ResizeOrdinal)
	recovered.until(func(rd *terminalReader) bool {
		return bytes.Contains(slices.Concat(state.Tail, rd.data), []byte("aaaEND"))
	})
	_ = tr.c.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": tr.workspaceID, "terminalId": id, "access": accessRead}, rpc.ErrorLimitExceeded)
}

func TestTerminalCloseCollectsFinalOutputEmittedDuringShutdown(t *testing.T) {
	tr := newTerminalRig(t, "trap 'printf final-words; exit 7' HUP\nprintf ready\nwhile :; do sleep 1; done\n")
	opened := tr.open(80, 24)
	id, att, lease := opened.Identity.TerminalID, opened.Attachment.AttachmentID, opened.Attachment.WriterLease
	rd := tr.reader(tr.c, id, att, "0", "0")
	rd.until(containing("ready"))
	tr.c.ok("dalgona/terminal/resize", map[string]any{"terminalId": id, "writerLease": lease, "resizeSeq": "1", "columns": 100, "rows": 30}, nil)
	final := closeTerminal(t, tr, id)
	checkCompleteFinalOutput(t, rd, final)
	checkExpiredWriteAfterClose(t, tr, id, lease)
	checkRepeatCloseStable(t, tr, id, final)
	viewer := checkDeniedWriteAttach(t, tr, id, att)
	checkFinalCheckpoint(t, tr, id, viewer, final)
	checkDetachedReadExpired(t, tr, id, viewer)
}

func closeTerminal(t *testing.T, tr *terminalRig, id string) terminalStatusWire {
	t.Helper()
	var closed struct {
		Status terminalStatusWire `json:"status"`
	}
	tr.c.ok("dalgona/terminal/close", map[string]any{"terminalId": id, "operationId": rpc.NewOperationID()}, &closed)
	final := closed.Status
	if final.State != terminalClosed || final.InputState != inputClosed || final.ExitCode == nil || *final.ExitCode != 7 {
		t.Fatalf("close status %+v, want a closed terminal that exited with code 7", final)
	}
	if final.FinalResizeOrdinal != "1" || final.OutputTruncated == nil || *final.OutputTruncated || final.OutputExpiresAt == "" {
		t.Fatalf("close status %+v, want complete final output through resize 1", final)
	}
	return final
}

func checkCompleteFinalOutput(t *testing.T, rd *terminalReader, final terminalStatusWire) {
	t.Helper()
	rd.until(func(rd *terminalReader) bool {
		return rd.offset == final.FinalOffset && rd.ordinal == final.FinalResizeOrdinal
	})
	if !bytes.HasSuffix(rd.data, []byte("final-words")) {
		t.Fatalf("collected final output %q does not end with the shutdown output", rd.data)
	}
}

func checkExpiredWriteAfterClose(t *testing.T, tr *terminalRig, id, lease string) {
	t.Helper()
	_ = tr.c.failsWith("dalgona/terminal/write", map[string]any{"terminalId": id, "writerLease": lease, "inputSeq": "1", "dataBase64": "eA=="}, rpc.ErrorExpired)
}

func checkRepeatCloseStable(t *testing.T, tr *terminalRig, id string, final terminalStatusWire) {
	t.Helper()
	var again struct {
		Status terminalStatusWire `json:"status"`
	}
	tr.c.ok("dalgona/terminal/close", map[string]any{"terminalId": id, "operationId": rpc.NewOperationID()}, &again)
	if again.Status.OutputExpiresAt != final.OutputExpiresAt || again.Status.FinalOffset != final.FinalOffset {
		t.Fatalf("repeated close changed the final status from %+v to %+v", final, again.Status)
	}
}

func checkDeniedWriteAttach(t *testing.T, tr *terminalRig, id, att string) terminalAttachedWire {
	t.Helper()
	_ = tr.c.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": tr.workspaceID, "terminalId": id, "access": accessWrite}, rpc.ErrorPermissionDenied)
	viewer := tr.attach(tr.c, tr.workspaceID, id, accessRead)
	tr.c.ok("dalgona/terminal/detach", map[string]any{"terminalId": id, "attachmentId": att}, nil)
	return viewer
}

func checkFinalCheckpoint(t *testing.T, tr *terminalRig, id string, viewer terminalAttachedWire, final terminalStatusWire) {
	t.Helper()
	var refreshed struct {
		Checkpoint terminalCheckpointWire `json:"checkpoint"`
	}
	tr.c.ok("dalgona/terminal/checkpoint/open", map[string]any{"terminalId": id, "attachmentId": viewer.Attachment.AttachmentID}, &refreshed)
	if refreshed.Checkpoint.NextOffset != final.FinalOffset || refreshed.Checkpoint.ResizeOrdinal != final.FinalResizeOrdinal {
		t.Fatalf("final checkpoint %+v does not sit at the final cursors %+v", refreshed.Checkpoint, final)
	}
	var detached struct {
		Status string `json:"status"`
	}
	tr.c.ok("dalgona/terminal/detach", map[string]any{"terminalId": id, "attachmentId": viewer.Attachment.AttachmentID}, &detached)
}

func checkDetachedReadExpired(t *testing.T, tr *terminalRig, id string, viewer terminalAttachedWire) {
	t.Helper()
	_ = tr.c.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": tr.workspaceID, "terminalId": id, "access": accessRead}, rpc.ErrorExpired)
}

func TestTerminalDetachReleasesOnlyThatAttachment(t *testing.T) {
	tr := newTerminalRig(t, "while read -r line; do printf 'got:%s;' \"$line\"; done\n")
	opened := tr.open(80, 24)
	id, writerAtt, lease := opened.Identity.TerminalID, opened.Attachment.AttachmentID, opened.Attachment.WriterLease
	viewer := tr.attach(tr.c, tr.workspaceID, id, accessRead)
	other := tr.r.connectNative(ServeOptions{Terminals: tr.host})
	otherWorkspace := tr.openWorkspace(other)
	detach := func(c *nativeClient, attachmentID, want string) {
		t.Helper()
		var detached struct {
			Status string `json:"status"`
		}
		c.ok("dalgona/terminal/detach", map[string]any{"terminalId": id, "attachmentId": attachmentID}, &detached)
		if detached.Status != want {
			t.Fatalf("detach %s = %q, want %q", attachmentID, detached.Status, want)
		}
	}
	detach(other, writerAtt, "notFound")
	_ = other.failsWith("dalgona/terminal/read", map[string]any{"terminalId": id, "attachmentId": writerAtt, "offset": "0", "afterResizeOrdinal": "0"}, rpc.ErrorExpired)
	detach(tr.c, viewer.Attachment.AttachmentID, "detached")
	detach(tr.c, viewer.Attachment.AttachmentID, "notFound")
	_ = tr.c.failsWith("dalgona/terminal/read", map[string]any{"terminalId": id, "attachmentId": viewer.Attachment.AttachmentID, "offset": "0", "afterResizeOrdinal": "0"}, rpc.ErrorExpired)
	tr.writeTo(id, lease, 1, "one\r")
	tr.reader(tr.c, id, writerAtt, "0", "0").until(containing("got:one;"))
	_ = other.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": otherWorkspace, "terminalId": id, "access": accessWrite}, rpc.ErrorBusy)
	detach(tr.c, writerAtt, "detached")
	successor := tr.attach(other, otherWorkspace, id, accessWrite)
	if successor.Attachment.WriterLease == "" || successor.Attachment.WriterLease == lease || successor.Status.State != terminalRunning {
		t.Fatalf("successor writer %+v status %+v, want a fresh lease on the running terminal", successor.Attachment, successor.Status)
	}
	other.ok("dalgona/terminal/write", map[string]any{"terminalId": id, "writerLease": successor.Attachment.WriterLease, "inputSeq": "1", "dataBase64": base64.StdEncoding.EncodeToString([]byte("two\r"))}, nil)
	tr.reader(other, id, successor.Attachment.AttachmentID, "0", "0").until(containing("got:two;"))
}

func TestTerminalRestartInvalidatesIdentifiers(t *testing.T) {
	tr := newTerminalRig(t, "while :; do sleep 1; done\n")
	opened := tr.open(80, 24)
	id, att := opened.Identity.TerminalID, opened.Attachment.AttachmentID
	restarted := tr.r.connectNative(ServeOptions{Terminals: NewTerminalHost(transcriptEngine)})
	workspace := tr.openWorkspace(restarted)
	_ = restarted.failsWith("dalgona/terminal/attach", map[string]any{"workspaceId": workspace, "terminalId": id, "access": accessWrite, "resumeLease": opened.Attachment.WriterLease}, rpc.ErrorExpired)
	_ = restarted.failsWith("dalgona/terminal/read", map[string]any{"terminalId": id, "attachmentId": att, "offset": "0", "afterResizeOrdinal": "0"}, rpc.ErrorExpired)
	_ = restarted.failsWith("dalgona/terminal/checkpoint/open", map[string]any{"terminalId": id, "attachmentId": att}, rpc.ErrorExpired)
	var listed struct {
		Terminals []jsontext.Value `json:"terminals"`
	}
	restarted.ok("dalgona/terminal/list", map[string]any{"workspaceId": workspace}, &listed)
	if len(listed.Terminals) != 0 {
		t.Fatalf("restarted daemon lists %d terminals, want none", len(listed.Terminals))
	}
	tr.c.ok("dalgona/terminal/list", map[string]any{"workspaceId": tr.workspaceID}, &listed)
	if len(listed.Terminals) != 1 {
		t.Fatalf("original daemon lists %d terminals, want its one terminal", len(listed.Terminals))
	}
}

func TestTerminalOperationsRejectChangedWorkspaceBinding(t *testing.T) {
	tr := newTerminalRig(t, "exec sleep 60\n")
	opened := tr.open(80, 24)
	id := opened.Identity.TerminalID
	attachID, lease := opened.Attachment.AttachmentID, opened.Attachment.WriterLease
	tr.c.ok("dalgona/terminal/read", map[string]any{"terminalId": id, "attachmentId": attachID, "offset": "0", "afterResizeOrdinal": "0"}, nil)
	thread, err := tr.r.host.Resume(t.Context(), tr.threadID)
	if err != nil {
		t.Fatal(err)
	}
	if err := thread.Append(t.Context(), event.RunnerBound{Runner: "box", Service: "docker", BoxID: "changed", Baseline: "base"}); err != nil {
		t.Fatal(err)
	}
	cases := []struct {
		method string
		params map[string]any
	}{
		{"read", map[string]any{"attachmentId": attachID, "offset": "0", "afterResizeOrdinal": "0"}},
		{"write", map[string]any{"writerLease": lease, "inputSeq": "1", "dataBase64": "eA=="}},
		{"resize", map[string]any{"writerLease": lease, "resizeSeq": "1", "columns": 90, "rows": 30}},
	}
	for _, tc := range cases {
		t.Run(tc.method, func(t *testing.T) {
			tc.params["terminalId"] = id
			response := tr.c.call("dalgona/terminal/"+tc.method, tc.params)
			if response.Error == nil {
				t.Fatalf("operation succeeded with %s after revocation", response.Result)
			}
			refusedWith(t, response.Error, rpc.ErrorExpired)
		})
	}
}
