package rpcserver

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type approvalAsk struct {
	thread    *agent.Thread
	requestID string
	method    string
	params    rpc.Encoder
	watch     *watch
}

func (s *server) ask(ctx context.Context, w *watch, ch <-chan agent.Update, update agent.Update) {
	prompt := update.Approval
	if prompt.Resolved {
		s.resolved(update.Thread, prompt.RequestID, prompt.Resolution)
		return
	}
	if s.requestRefused(prompt.RequestID) {
		s.refuseReplayedApproval(w, ch, prompt.RequestID)
		return
	}
	turnID := w.tracker.currentTurn()
	threadID := update.Thread
	if threadID != w.thread.ID() {
		turnID = ""
	}
	method, params := approvalParams(prompt, threadID, turnID, approvalCwd(prompt, w))
	a := approvalAsk{thread: w.thread, requestID: prompt.RequestID, method: method, params: params, watch: w}
	note := approvalNotice(prompt, threadID, turnID)
	askCtx, cancel := context.WithCancel(ctx)
	started, dup := s.spawnRequest(prompt.RequestID, threadID, cancel, func() {
		defer s.settle(prompt.RequestID)
		if note != nil {
			s.send("item/fileChange/patchUpdated", *note)
		}
		s.request(ctx, askCtx, a)
	})
	if dup || !started {
		cancel()
	}
	if started && !dup {
		recordApprovalNote(w, threadID, note)
	}
}

func (s *server) refuseReplayedApproval(w *watch, ch <-chan agent.Update, requestID string) {
	if err := w.thread.CannotAnswerApproval(requestID, ch); err != nil && !errors.Is(err, agent.ErrUnknownRequest) {
		s.log.Warn("rpc: report refused replayed approval", "request", requestID, "error", err)
	}
}

func recordApprovalNote(w *watch, threadID string, note *rpc.FileChangePatchUpdatedNotification) {
	if note == nil {
		return
	}
	recordApprovalChanges(w, threadID, *note)
}

func approvalParams(prompt agent.ApprovalPrompt, threadID, turnID, cwd string) (string, rpc.Encoder) {
	if prompt.Class == tool.ClassEdit {
		return "item/fileChange/requestApproval", rpc.FileChangeRequestApprovalParams{
			ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, StartedAtMs: nowMillis(), Reason: prompt.Summary,
		}
	}
	return "item/commandExecution/requestApproval", rpc.CommandExecutionRequestApprovalParams{
		ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, StartedAtMs: nowMillis(),
		Command: cmdText(prompt), Cwd: cwd, Reason: prompt.Reason, Escalated: prompt.Escalated,
	}
}

func approvalCwd(prompt agent.ApprovalPrompt, w *watch) string {
	if len(prompt.Paths) > 0 && prompt.Tool == "mcp_workspace" && prompt.Paths[0] != "" {
		return prompt.Paths[0]
	}
	if it, ok := w.tracker.items[prompt.CallID]; ok && it.Cwd != "" {
		return it.Cwd
	}
	return w.tracker.created.Cwd
}

func (s *server) spawn(register, task func()) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false
	}
	register()
	s.tasks.Go(task)
	return true
}

func (s *server) spawnRequest(requestID, threadID string, cancel context.CancelFunc, task func()) (started, dup bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false, false
	}
	if _, exists := s.pending[requestID]; exists {
		return true, true
	}
	s.pending[requestID] = cancel
	s.threads[requestID] = threadID
	s.tasks.Go(task)
	return true, false
}

func cmdText(prompt agent.ApprovalPrompt) string {
	return cmp.Or(prompt.Command, strings.Join(prompt.Commands, "\n"), prompt.Summary)
}

func (s *server) settle(requestID string) {
	s.mu.Lock()
	cancel := s.takeRequestLocked(requestID)
	s.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

func (s *server) takeRequestLocked(requestID string) context.CancelFunc {
	cancel := s.pending[requestID]
	delete(s.pending, requestID)
	delete(s.threads, requestID)
	return cancel
}

func (s *server) resolved(threadID, requestID, resolution string) {
	s.settle(requestID)
	s.unrefuseRequest(requestID)
	s.send("serverRequest/resolved", rpc.ServerRequestResolvedNotification{ThreadID: threadID, RequestID: requestID, Resolution: resolution})
}

func (s *server) request(serveCtx, askCtx context.Context, a approvalAsk) {
	params, err := rpc.Marshal(a.params)
	if err != nil {
		s.log.Warn("rpc: encode approval request", "request", a.requestID, "error", err)
		s.answer(a, agent.ReplyDecline)
		return
	}
	var response jsontext.Value
	err = s.conn.CallID(askCtx, a.requestID, a.method, params, &response)
	switch {
	case err == nil:
		s.answer(a, decisionReply(response))
	case serveCtx.Err() != nil, errors.Is(err, io.ErrClosedPipe):
	case askCtx.Err() != nil:
	default:
		s.cannotAnswerApproval(a, err)
	}
}

func (s *server) cannotAnswerApproval(a approvalAsk, cause error) {
	s.log.Info("rpc: approval request failed; this window cannot answer", "request", a.requestID, "error", cause)
	s.refuseRequest(a.requestID)
	if err := a.thread.CannotAnswerApproval(a.requestID, a.watch.channel()); err != nil && !errors.Is(err, agent.ErrUnknownRequest) {
		s.log.Warn("rpc: report unanswerable approval", "request", a.requestID, "error", err)
	}
}

func (s *server) answer(a approvalAsk, reply agent.Reply) {
	err := a.thread.Answer(a.requestID, reply)
	if err != nil && !errors.Is(err, agent.ErrUnknownRequest) {
		s.log.Warn("rpc: answer approval", "request", a.requestID, "error", err)
	}
}

func decisionReply(response jsontext.Value) agent.Reply {
	reply := agent.ReplyDecline
	_ = decodeParams(response, func(name string, d *jsontext.Decoder) error {
		if name != "decision" {
			return d.SkipValue()
		}
		if d.PeekKind() == '{' {
			reply = agent.ReplyAccept
			return d.SkipValue()
		}
		value, err := rpc.ReadString(d)
		reply = replyFor(value)
		return err
	})
	return reply
}

func replyFor(decision string) agent.Reply {
	switch decision {
	case "accept", "approved":
		return agent.ReplyAccept
	case "acceptForSession", "approved_for_session":
		return agent.ReplyAcceptSession
	case "cancel", "abort":
		return agent.ReplyCancel
	}
	return agent.ReplyDecline
}

func approvalNotice(prompt agent.ApprovalPrompt, threadID, turnID string) *rpc.FileChangePatchUpdatedNotification {
	if prompt.Class != tool.ClassEdit {
		return nil
	}
	note := rpc.FileChangePatchUpdatedNotification{ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, Changes: approvalChanges(prompt.Files)}
	return &note
}

func recordApprovalChanges(w *watch, threadID string, note rpc.FileChangePatchUpdatedNotification) {
	if item, ok := w.tracker.items[note.ItemID]; ok && threadID == w.thread.ID() {
		item.Changes = note.Changes
	}
}

func approvalChanges(files []agent.ApprovalFile) []rpc.FileChange {
	changes := make([]rpc.FileChange, 0, len(files))
	for _, file := range files {
		kind := approvalKind(file)
		if file.TooLarge {
			changes = append(changes, omittedChange(file.Path, kind))
			continue
		}
		changes = append(changes, diffChange(file.Path, kind, file.Pre, file.Post))
	}
	return changes
}

func approvalKind(file agent.ApprovalFile) string {
	switch {
	case file.Added:
		return rpc.KindAdd
	case file.Deleted:
		return rpc.KindDelete
	}
	return rpc.KindUpdate
}
