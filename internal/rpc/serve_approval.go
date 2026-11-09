package rpc

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"strings"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/tool"
)

type approvalAsk struct {
	thread    *agent.Thread
	requestID string
	method    string
	params    writer
}

func (s *server) ask(ctx context.Context, w *watch, update agent.Update) {
	prompt := update.Approval
	if prompt.Resolved {
		s.resolved(update.Thread, prompt.RequestID)
		return
	}
	turnID := w.tracker.currentTurn()
	cwd := approvalCwd(prompt, w)
	threadID := update.Thread
	if threadID != w.thread.ID() {
		turnID = ""
	}
	a := approvalAsk{thread: w.thread, requestID: prompt.RequestID}
	if prompt.Class == tool.ClassEdit {
		a.method = "item/fileChange/requestApproval"
		a.params = FileChangeRequestApprovalParams{
			ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, StartedAtMs: nowMillis(), Reason: prompt.Summary,
		}
	} else {
		a.method = "item/commandExecution/requestApproval"
		a.params = CommandExecutionRequestApprovalParams{
			ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, StartedAtMs: nowMillis(),
			Command: cmdText(prompt), Cwd: cwd, Reason: prompt.Reason,
		}
	}
	askCtx, cancel := context.WithCancel(ctx)
	started, dup := s.spawnApproval(prompt.RequestID, cancel, func() {
		defer s.settle(prompt.RequestID)
		s.request(ctx, askCtx, a)
	})
	if dup {
		cancel()
		return
	}
	if !started {
		cancel()
		s.declineIfAlone(a)
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

func (s *server) spawnApproval(requestID string, cancel context.CancelFunc, task func()) (started, dup bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return false, false
	}
	if _, exists := s.pending[requestID]; exists {
		return true, true
	}
	s.pending[requestID] = cancel
	s.tasks.Go(task)
	return true, false
}

func cmdText(prompt agent.ApprovalPrompt) string {
	if len(prompt.Commands) == 0 {
		return prompt.Summary
	}
	return strings.Join(prompt.Commands, " && ")
}

func (s *server) settle(requestID string) {
	s.mu.Lock()
	cancel := s.pending[requestID]
	delete(s.pending, requestID)
	s.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

func (s *server) resolved(threadID, requestID string) {
	s.settle(requestID)
	s.send("serverRequest/resolved", ServerRequestResolvedNotification{ThreadID: threadID, RequestID: requestID})
}

func (s *server) request(serveCtx, askCtx context.Context, a approvalAsk) {
	params, err := encode(a.params)
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
		s.declineIfAlone(a)
	case askCtx.Err() != nil:
	default:
		s.log.Info("rpc: approval request failed; declining", "request", a.requestID, "error", err)
		s.answer(a, agent.ReplyDecline)
	}
}

func (s *server) declineIfAlone(a approvalAsk) {
	status, err := a.thread.Status()
	if err != nil || status.Subscribers > 1 {
		return
	}
	s.answer(a, agent.ReplyDecline)
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
		value, err := readString(d)
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
