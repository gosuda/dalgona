package agent

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/rules"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	declineNoClient = "declined: no client could approve this call"
	taskLimit       = 2000
	summaryLimit    = 200
)

type gateCall struct {
	call          event.ToolCall
	args          jsontext.Value
	class         tool.Class
	command       string
	escalate      bool
	justification string
	paths         []string
	outside       []string
	requestID     string
}

type verdict struct {
	allow       bool
	decision    string
	by          string
	reason      string
	scores      map[string]float64
	unsandboxed bool
	cancel      bool
}

func accept(by string) verdict {
	return verdict{allow: true, decision: "accept", by: by, reason: "allowed"}
}

func decline(by, reason string) verdict { return verdict{decision: "decline", by: by, reason: reason} }

func (w *worker) gate(ctx context.Context, gc gateCall) (verdict, error) {
	requestID := gc.requestID
	if requestID == "" {
		requestID = newID("req-")
		requested := event.ApprovalRequested{RequestID: requestID, CallID: gc.call.ID, Summary: summary(gc), Class: classString(gc.class)}
		if prefix, ok := grantPrefixFor(gc); ok {
			requested.GrantPrefix = prefix
		}
		if _, err := w.t.appendEvents(ctx, requested); err != nil {
			return verdict{}, err
		}
	}
	v := w.decide(ctx, gc, requestID)
	resolved := event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason, Scores: v.scores}
	_, err := w.t.appendEvents(ctx, resolved)
	return v, err
}

func summary(gc gateCall) string {
	switch {
	case gc.class == tool.ClassExec && gc.command != "":
		return truncate(gc.command, summaryLimit)
	case len(gc.paths) > 0:
		return truncate(gc.call.Name+" "+strings.Join(gc.paths, ", "), summaryLimit)
	default:
		return truncate(gc.call.Name+" "+string(gc.args), summaryLimit)
	}
}

func truncate(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	return strings.ToValidUTF8(text[:limit], "")
}

func (w *worker) decide(ctx context.Context, gc gateCall, requestID string) verdict {
	if v, ok := w.hookVerdict(ctx, gc); ok {
		return v
	}
	approval := w.settings.Approval
	if approval == config.ApprovalYolo {
		v := accept("mode:yolo")
		v.unsandboxed = true
		return v
	}
	if v, ok := mechanical(gc, approval); ok {
		return v
	}
	if v, ok := staticRule(gc); ok {
		return v
	}
	if approval == config.ApprovalAsk {
		return w.human(ctx, gc, requestID)
	}
	return w.automated(ctx, gc)
}

func (w *worker) hookVerdict(ctx context.Context, gc gateCall) (verdict, bool) {
	call := tool.Call{ID: gc.call.ID, Name: gc.call.Name, Args: gc.args}
	result, err := w.t.ext.ToolCall(withWorker(ctx, w), call)
	if err != nil {
		w.t.host.log.Warn("agent: tool_call hook", "thread", w.t.id, "tool", gc.call.Name, "error", err)
	}
	switch result.Decision {
	case HookAllow:
		return accept(result.By), true
	case HookDeny:
		return decline(result.By, cmp.Or(result.Reason, "declined by "+result.By)), true
	}
	return verdict{}, false
}

func mechanical(gc gateCall, approval config.Approval) (verdict, bool) {
	by := "mode:" + string(approval)
	switch gc.class {
	case tool.ClassRead, tool.ClassAgent, tool.ClassScript:
		return accept(by), true
	case tool.ClassEdit:
		return accept(by), approval == config.ApprovalAuto && len(gc.outside) == 0
	case tool.ClassExec, tool.ClassRemote:
		return verdict{}, false
	}
	return verdict{}, false
}

func staticRule(gc gateCall) (verdict, bool) {
	if gc.class == tool.ClassRemote {
		return staticRemoteRule(gc)
	}
	if gc.class != tool.ClassExec {
		return verdict{}, false
	}
	name, part, matched := rules.Check(gc.command)
	if !matched {
		return verdict{}, false
	}
	return decline("rule:"+name, "declined by rule "+name+": "+part), true
}

func staticRemoteRule(gc gateCall) (verdict, bool) {
	for _, value := range remoteStrings(string(gc.args)) {
		if name, part, ok := rules.CheckValue(value); ok {
			return decline("rule:"+name, "declined by rule "+name+": "+part), true
		}
	}
	return verdict{}, false
}

func remoteStrings(raw string) []string {
	var decoded any
	if err := json.Unmarshal([]byte(raw), &decoded); err != nil {
		return nil
	}
	var out []string
	var walk func(value any)
	walk = func(value any) {
		switch value := value.(type) {
		case string:
			out = append(out, value)
		case []any:
			for _, item := range value {
				walk(item)
			}
		case map[string]any:
			for _, item := range value {
				walk(item)
			}
		}
	}
	walk(decoded)
	return out
}

func (w *worker) automated(ctx context.Context, gc gateCall) verdict {
	decider := w.t.host.cfg.Decider
	if decider == nil {
		return decline("decider", "declined by decider: no decider is configured")
	}
	decision, err := decider.Decide(ctx, w.action(gc))
	if err != nil {
		return decline("decider", "declined by decider: "+err.Error())
	}
	by := cmp.Or(decision.By, "decider")
	if !decision.Allow {
		v := decline(by, cmp.Or(decision.Reason, "declined by decider"))
		v.scores = decision.Scores
		return v
	}
	v := accept(by)
	v.reason, v.scores, v.unsandboxed = cmp.Or(decision.Reason, v.reason), decision.Scores, gc.escalate
	return v
}

func (w *worker) action(gc gateCall) Action {
	a := Action{
		Thread: w.t.id, Tool: gc.call.Name, Class: gc.class, Paths: gc.paths, Project: w.t.created.Cwd,
		Task: w.task(), Justification: gc.justification, Sandboxed: w.sandboxed(gc),
	}
	switch gc.class {
	case tool.ClassExec:
		a.Commands, a.Text = rules.Split(gc.command), gc.command
	case tool.ClassRemote:
		a.Text = gc.call.Name + " " + string(gc.args)
		a.Commands = []string{a.Text}
	case tool.ClassEdit:
		for _, path := range gc.outside {
			a.Commands = append(a.Commands, "write "+path)
		}
	case tool.ClassRead, tool.ClassAgent, tool.ClassScript:
	}
	return a
}

func (w *worker) sandboxed(gc gateCall) bool {
	return !gc.escalate && w.settings.Sandbox != config.SandboxOff && runner.Available()
}

func (w *worker) task() string {
	v, err := w.t.view()
	if err != nil {
		return ""
	}
	path := event.ActivePath(v.records)
	for i := len(path) - 1; i >= 0; i-- {
		message, ok := path[i].Event.(event.UserMessage)
		if !ok {
			continue
		}
		text, err := w.t.host.loadText(message.Text, message.TextBlob)
		if err != nil {
			w.t.host.log.Warn("agent: load task text", "thread", w.t.id, "error", err)
			return ""
		}
		return truncate(text, taskLimit)
	}
	return ""
}

func (w *worker) human(ctx context.Context, gc gateCall, requestID string) verdict {
	if w.t.created.Advisor != "" {
		return w.automated(ctx, gc)
	}
	if prefix := w.matchingGrant(gc); prefix != "" {
		return accept("grant:" + prefix)
	}
	replies := make(chan Reply, 1)
	prompt := ApprovalPrompt{
		RequestID: requestID, CallID: gc.call.ID, Tool: gc.call.Name, Class: gc.class, Summary: summary(gc), Paths: gc.paths,
		Reason: gc.justification,
	}
	prompt.Summary = withRunContext(prompt.Summary, gc, w.t.created.Cwd)
	if gc.class == tool.ClassExec {
		prompt.Commands = rules.Split(gc.command)
	}
	offered, err := ask(w.t, func(a *actor) bool { return a.offerApproval(prompt, replies) })
	forwarded, linked := w.t.forwardApproval(prompt)
	v := decline("mode:ask", declineNoClient)
	if err == nil && (forwarded || (offered && !linked)) {
		v = awaitReply(ctx, replies)
	}
	if forwarded {
		prompt.Resolved = true
		w.t.forwardApproval(prompt)
	}
	return v
}

func awaitReply(ctx context.Context, replies <-chan Reply) verdict {
	select {
	case reply := <-replies:
		return replyVerdict(reply)
	case <-ctx.Done():
		return verdict{decision: "cancel", by: "user", reason: "the turn was interrupted"}
	}
}

func (a *actor) offerApproval(prompt ApprovalPrompt, replies chan Reply) bool {
	a.approvals[prompt.RequestID] = replies
	a.publish(Update{Kind: UpdateApproval, Thread: a.t.id, Approval: prompt})
	return len(a.subs) > 0
}

func (t *Thread) forwardApproval(prompt ApprovalPrompt) (reached, linked bool) {
	parent, open := t.host.loadedThread(t.created.Parent)
	if !open || t.created.Advisor != "" {
		return false, false
	}
	reached, err := ask(parent, func(a *actor) bool {
		a.publish(Update{Kind: UpdateApproval, Thread: t.id, Approval: prompt})
		return len(a.subs) > 0
	})
	return err == nil && reached, true
}

func replyVerdict(reply Reply) verdict {
	switch reply {
	case ReplyAccept:
		return accept("user")
	case ReplyAcceptSession:
		return verdict{allow: true, decision: "accept_session", by: "user", reason: "allowed for this session"}
	case ReplyCancel:
		return verdict{decision: "cancel", by: "user", reason: "cancelled by user", cancel: true}
	case ReplyDecline:
		return decline("user", "declined by user")
	}
	return decline("user", "declined by user")
}

func (w *worker) matchingGrant(gc gateCall) string {
	v, err := w.t.view()
	if err != nil {
		return ""
	}
	if gc.class != tool.ClassExec {
		key := "tool:" + gc.call.Name
		if _, ok := v.state.Grants[key]; ok {
			return key
		}
		return ""
	}
	for key := range v.state.Grants {
		if strings.HasPrefix(key, "tool:") {
			continue
		}
		prefix, escalated := splitGrantKey(key)
		if escalated != gc.escalate {
			continue
		}
		if rules.MatchesGrant(gc.command, prefix) {
			return prefix
		}
	}
	return ""
}

const grantEscalatedSuffix = "\x00escalate"

func splitGrantKey(key string) (string, bool) {
	if prefix, ok := strings.CutSuffix(key, grantEscalatedSuffix); ok {
		return prefix, true
	}
	return key, false
}

func withRunContext(summary string, gc gateCall, cwd string) string {
	if gc.class == tool.ClassExec && gc.escalate {
		summary += " [unsandboxed]"
	}
	if cwd != "" {
		summary += " [cwd " + cwd + "]"
	}
	return summary
}

func grantPrefixFor(gc gateCall) (string, bool) {
	if gc.class != tool.ClassExec || gc.command == "" {
		return "", false
	}
	prefix, ok := rules.GrantPrefix(gc.command)
	if !ok {
		return "", false
	}
	if gc.escalate {
		prefix += grantEscalatedSuffix
	}
	return prefix, true
}

func (h *Host) outsideRoots(cwd string, paths []string) []string {
	roots := append([]string{resolveExisting(cwd)}, h.roots...)
	var outside []string
	for _, path := range paths {
		if !filepath.IsAbs(path) {
			path = filepath.Join(cwd, path)
		}
		if !insideRoots(roots, resolveExisting(filepath.Clean(path))) {
			outside = append(outside, path)
		}
	}
	return outside
}

func insideRoots(roots []string, path string) bool {
	for _, root := range roots {
		rel, err := filepath.Rel(root, path)
		if err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && !filepath.IsAbs(rel) {
			return true
		}
	}
	return false
}

func resolveExisting(path string) string {
	suffix := ""
	for current := path; ; current = filepath.Dir(current) {
		if real, err := filepath.EvalSymlinks(current); err == nil {
			return filepath.Join(real, suffix)
		}
		parent := filepath.Dir(current)
		if parent == current {
			return path
		}
		suffix = filepath.Join(filepath.Base(current), suffix)
	}
}
