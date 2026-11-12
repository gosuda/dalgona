package rpc

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
)

const defaultListLimit = 50

func nowMillis() int64 { return time.Now().UnixMilli() }

func unixSeconds(t time.Time) int64 {
	if t.IsZero() {
		return 0
	}
	return t.Unix()
}

func provider(model string) string {
	name, _, _ := strings.Cut(model, "/")
	return name
}

func approvalMode(policy string) (config.Approval, error) {
	switch policy {
	case "":
		return "", nil
	case "never", string(config.ApprovalYolo):
		return config.ApprovalYolo, nil
	case "on-request", "on-failure", string(config.ApprovalAuto):
		return config.ApprovalAuto, nil
	case "untrusted", string(config.ApprovalAsk):
		return config.ApprovalAsk, nil
	}
	return "", invalidParams("unsupported approvalPolicy " + policy)
}

func readApprovalPolicy(d *jsontext.Decoder) (string, error) {
	if d.PeekKind() != jsontext.KindBeginObject {
		return readString(d)
	}
	granular := false
	err := readMembers(d, func(name string, d *jsontext.Decoder) error {
		granular = granular || name == "granular"
		return d.SkipValue()
	})
	if err == nil && !granular {
		err = errors.New("an approval policy object must be granular")
	}
	return string(config.ApprovalAsk), err
}

func sandboxMode(mode string) (config.SandboxMode, error) {
	switch mode {
	case "":
		return "", nil
	case "danger-full-access", string(config.SandboxOff):
		return config.SandboxOff, nil
	case "workspace-write", "read-only":
		return config.SandboxWorkspaceWrite, nil
	}
	return "", invalidParams("unsupported sandbox " + mode)
}

func policyName(approval string) string {
	switch config.Approval(approval) {
	case config.ApprovalYolo:
		return "never"
	case config.ApprovalAsk:
		return "untrusted"
	case config.ApprovalAuto:
		return "on-request"
	}
	return "on-request"
}

func sandboxModeName(sandbox string) string {
	if config.SandboxMode(sandbox) == config.SandboxOff {
		return "danger-full-access"
	}
	return "workspace-write"
}

func sandboxPolicy(sandbox string) SandboxPolicy {
	if config.SandboxMode(sandbox) == config.SandboxOff {
		return SandboxPolicy{Type: "dangerFullAccess"}
	}
	return SandboxPolicy{Type: "workspaceWrite"}
}

func effortName(reasoning config.Reasoning) string {
	if reasoning == config.ReasoningDefault {
		return ""
	}
	return string(reasoning)
}

func effortMode(value string) (config.Reasoning, error) {
	switch config.Reasoning(value) {
	case "":
		return "", nil
	case config.ReasoningLow, config.ReasoningMedium, config.ReasoningHigh, config.ReasoningXHigh, config.ReasoningMax:
		return config.Reasoning(value), nil
	case config.ReasoningDefault:
		return "", invalidParams("unsupported model_reasoning_effort " + value)
	}
	return "", invalidParams("unsupported model_reasoning_effort " + value)
}

func threadStatus(thread *agent.Thread) ThreadStatus {
	status, err := thread.Status()
	if err != nil {
		return ThreadStatus{Type: "systemError"}
	}
	switch status.State {
	case event.StatusTurnRunning:
		if status.Questions > 0 {
			return ThreadStatus{Type: "active", ActiveFlags: []string{"waitingOnUserInput"}}
		}
		return ThreadStatus{Type: "active", ActiveFlags: []string{}}
	case event.StatusAwaitingApproval:
		return ThreadStatus{Type: "active", ActiveFlags: []string{"waitingOnApproval"}}
	case event.StatusDamaged:
		return ThreadStatus{Type: "systemError"}
	case event.StatusIdle:
	}
	return ThreadStatus{Type: "idle"}
}

func (s *server) info(id string) (agent.ThreadInfo, bool) {
	for _, info := range s.host.AllThreads() {
		if info.ID == id {
			return info, true
		}
	}
	return agent.ThreadInfo{}, false
}

func (s *server) threadObject(info agent.ThreadInfo, status ThreadStatus, turns []Turn) Thread {
	if turns == nil {
		turns = []Turn{}
	}
	return Thread{
		ID: info.ID, SessionID: info.ID, Name: info.Name, Preview: info.Preview,
		Model: info.Model, ModelProvider: provider(info.Model),
		CreatedAt: unixSeconds(info.Created), UpdatedAt: unixSeconds(info.Updated),
		Cwd: info.Cwd, CLIVersion: config.Version(), Source: "appServer", Status: status,
		ParentThreadID: info.Parent, ForkedFromID: info.ForkOf, Turns: turns,
	}
}

func (s *server) open(ctx context.Context, id string) (*agent.Thread, error) {
	if id == "" {
		return nil, invalidParams("threadId is required")
	}
	if _, ok := s.info(id); !ok {
		return nil, unknownThread(id)
	}
	thread, err := s.host.Resume(ctx, id)
	if err != nil {
		return nil, hostError(err, id, "")
	}
	return thread, nil
}

func (s *server) watched(ctx context.Context, id string) (*agent.Thread, error) {
	thread, err := s.open(ctx, id)
	if err != nil {
		return nil, err
	}
	_, _, err = s.watch(ctx, thread)
	return thread, err
}

func (s *server) session(thread *agent.Thread, snap snapshot) (ThreadStartResponse, error) {
	info, ok := s.info(thread.ID())
	if !ok {
		return ThreadStartResponse{}, unknownThread(thread.ID())
	}
	current, err := thread.Settings()
	if err != nil {
		return ThreadStartResponse{}, hostError(err, thread.ID(), "")
	}
	effective := current.Sandbox
	if resolved, resolveErr := thread.EffectiveSandbox(); resolveErr == nil {
		effective = resolved
	}
	return ThreadStartResponse{
		ApprovalPolicy: policyName(string(current.Approval)), ApprovalsReviewer: "user",
		Cwd: snap.created.Cwd, Model: current.Model, ModelProvider: provider(current.Model),
		Sandbox:         sandboxPolicy(string(effective)),
		ReasoningEffort: cmp.Or(effortName(current.Effort), effortName(s.host.Config().Reasoning)),
		TokenUsage:      snap.usage,
		Thread:          s.threadObject(info, threadStatus(thread), snap.turns),
	}, nil
}

func (s *server) threadStart(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadStartParams
	if err := decodeParams(params, p.member); err != nil {
		return nil, err
	}
	spec, err := s.spec(p)
	if err != nil {
		return nil, err
	}
	thread, err := s.host.Start(ctx, spec)
	if err != nil {
		return nil, invalidRequest(err.Error())
	}
	return s.announce(ctx, thread)
}

func (s *server) spec(p ThreadStartParams) (agent.ThreadSpec, error) {
	approval, err := approvalMode(p.ApprovalPolicy)
	if err != nil {
		return agent.ThreadSpec{}, err
	}
	sandbox, err := sandboxMode(p.Sandbox)
	if err != nil {
		return agent.ThreadSpec{}, err
	}
	var effort config.Reasoning
	if p.Config != nil {
		if effort, err = effortMode(p.Config.ModelReasoningEffort); err != nil {
			return agent.ThreadSpec{}, err
		}
	}
	cwd := cmp.Or(p.Cwd, s.host.Config().Cwd)
	if cwd != "" {
		if cwd, err = filepath.Abs(cwd); err != nil {
			return agent.ThreadSpec{}, invalidParams("cwd: " + err.Error())
		}
	}
	current := s.host.Config()
	return agent.ThreadSpec{
		Cwd: cwd, Model: qualifiedModel(p.Model, p.ModelProvider, current.Model),
		Approval: cmp.Or(approval, current.Approval), Sandbox: cmp.Or(sandbox, current.Sandbox),
		Effort: effort,
	}, nil
}

func qualifiedModel(model, modelProvider, current string) string {
	if model == "" {
		return current
	}
	if strings.Contains(model, "/") {
		return model
	}
	name := cmp.Or(modelProvider, provider(current))
	if name == "" {
		return model
	}
	return name + "/" + model
}

func (s *server) announce(ctx context.Context, thread *agent.Thread) (jsontext.Value, error) {
	snap, err := s.attach(ctx, thread)
	if err != nil {
		return nil, err
	}
	response, err := s.session(thread, snap)
	if err != nil {
		return nil, err
	}
	s.send("thread/started", ThreadStartedNotification{Thread: response.Thread})
	return encode(response)
}

func (p *ThreadStartParams) member(name string, d *jsontext.Decoder) error {
	if name == "approvalPolicy" {
		v, err := readApprovalPolicy(d)
		p.ApprovalPolicy = v
		return err
	}
	if name == "config" {
		return readMembers(d, p.configMember)
	}
	var target *string
	switch name {
	case "model":
		target = &p.Model
	case "sandbox":
		target = &p.Sandbox
	case "cwd":
		target = &p.Cwd
	case "modelProvider":
		target = &p.ModelProvider
	default:
		return d.SkipValue()
	}
	v, err := readString(d)
	*target = v
	return err
}

func (p *ThreadStartParams) configMember(name string, d *jsontext.Decoder) error {
	if name != "model_reasoning_effort" {
		return d.SkipValue()
	}
	if p.Config == nil {
		p.Config = &ThreadStartConfig{}
	}
	v, err := readString(d)
	p.Config.ModelReasoningEffort = v
	return err
}

func threadIDMember(id *string) memberFunc {
	return func(name string, d *jsontext.Decoder) error {
		if name != "threadId" {
			return d.SkipValue()
		}
		v, err := readString(d)
		*id = v
		return err
	}
}

func stringMembers(fields map[string]*string) memberFunc {
	return func(name string, d *jsontext.Decoder) error {
		target, ok := fields[name]
		if !ok {
			return d.SkipValue()
		}
		v, err := readString(d)
		*target = v
		return err
	}
}

func (s *server) threadResume(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return nil, err
	}
	thread, err := s.open(ctx, id)
	if err != nil {
		return nil, err
	}
	snap, err := s.attach(ctx, thread)
	if err != nil {
		return nil, err
	}
	response, err := s.session(thread, snap)
	if err != nil {
		return nil, err
	}
	return encode(response)
}

func (s *server) threadList(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadListParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "cursor":
			p.Cursor, err = readString(d)
		case "limit":
			p.Limit, err = readInt(d)
			p.HasLimit = err == nil
		case "archived":
			p.Archived, err = readBool(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	if err != nil {
		return nil, err
	}
	return s.listThreads(p)
}

func (s *server) listThreads(p ThreadListParams) (jsontext.Value, error) {
	infos := slices.DeleteFunc(s.host.AllThreads(), func(info agent.ThreadInfo) bool { return info.Archived != p.Archived })
	slices.SortStableFunc(infos, func(a, b agent.ThreadInfo) int { return b.Updated.Compare(a.Updated) })
	start, err := cursorOffset(p.Cursor)
	if err != nil {
		return nil, err
	}
	start = min(start, len(infos))
	if p.Limit < 0 {
		return nil, invalidParams("invalid limit " + strconv.Itoa(p.Limit))
	}
	limit := p.Limit
	if !p.HasLimit {
		limit = defaultListLimit
	}
	end := cappedEnd(start, limit, len(infos))
	response := ThreadListResponse{Data: make([]Thread, 0, end-start)}
	for _, info := range infos[start:end] {
		response.Data = append(response.Data, s.threadObject(info, s.listedStatus(info), nil))
	}
	if end < len(infos) {
		response.NextCursor = strconv.Itoa(end)
	}
	return encode(response)
}

func cappedEnd(start, limit, total int) int {
	if limit > total-start {
		return total
	}
	return start + limit
}

func cursorOffset(cursor string) (int, error) {
	if cursor == "" {
		return 0, nil
	}
	offset, err := strconv.Atoi(cursor)
	if err != nil || offset < 0 {
		return 0, invalidParams("invalid cursor " + cursor)
	}
	return offset, nil
}

func (s *server) listedStatus(info agent.ThreadInfo) ThreadStatus {
	if info.Damaged {
		return ThreadStatus{Type: "systemError"}
	}
	s.mu.Lock()
	w, ok := s.watches[info.ID]
	s.mu.Unlock()
	if ok {
		return threadStatus(w.thread)
	}
	return ThreadStatus{Type: "notLoaded"}
}

func (s *server) threadRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadReadParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name == "includeTurns" {
			v, err := readBool(d)
			p.IncludeTurns = v
			return err
		}
		return threadIDMember(&p.ThreadID)(name, d)
	})
	if err != nil {
		return nil, err
	}
	info, ok := s.info(p.ThreadID)
	if !ok {
		return nil, unknownThread(p.ThreadID)
	}
	if !p.IncludeTurns {
		return encode(ThreadReadResponse{Thread: s.threadObject(info, s.listedStatus(info), nil)})
	}
	thread, err := s.open(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	snap, err := s.snapshot(thread)
	if err != nil {
		return nil, err
	}
	return encode(ThreadReadResponse{Thread: s.threadObject(info, threadStatus(thread), snap.turns)})
}

func (s *server) threadNameSet(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadNameSetParams
	if err := decodeParams(params, stringMembers(map[string]*string{"threadId": &p.ThreadID, "name": &p.Name})); err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	if err := thread.SetName(ctx, p.Name); err != nil {
		return nil, invalidParams(err.Error())
	}
	return emptyObject(), nil
}

func (s *server) threadArchive(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	if _, err := s.setArchived(ctx, params, s.archive, "thread/archived"); err != nil {
		return nil, err
	}
	return emptyObject(), nil
}

func (s *server) archive(ctx context.Context, id string) error {
	if err := s.host.Archive(ctx, id); err != nil {
		return err
	}
	before := s.opts.BeforeArchive
	if before == nil {
		return nil
	}
	hookErr := before(ctx, id)
	if hookErr == nil {
		return nil
	}
	if undoErr := s.host.Unarchive(ctx, id); undoErr != nil {
		return fmt.Errorf("archive hook: %w", errors.Join(hookErr, fmt.Errorf("restore thread: %w", undoErr)))
	}
	return fmt.Errorf("archive hook: %w", hookErr)
}

func (s *server) threadUnarchive(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	info, err := s.setArchived(ctx, params, s.host.Unarchive, "thread/unarchived")
	if err != nil {
		return nil, err
	}
	return encode(ThreadReadResponse{Thread: s.threadObject(info, s.listedStatus(info), nil)})
}

func (s *server) setArchived(ctx context.Context, params jsontext.Value, apply func(context.Context, string) error, method string) (agent.ThreadInfo, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return agent.ThreadInfo{}, err
	}
	if _, ok := s.info(id); !ok {
		return agent.ThreadInfo{}, unknownThread(id)
	}
	if err := apply(ctx, id); err != nil {
		return agent.ThreadInfo{}, hostError(err, id, "")
	}
	if !s.watching(id) {
		s.send(method, ThreadRevertedNotification{ThreadID: id})
	}
	info, _ := s.info(id)
	return info, nil
}

func (s *server) threadCompact(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := thread.Compact(ctx); err != nil {
		return nil, hostError(err, id, "")
	}
	return emptyObject(), nil
}

func (s *server) threadRevert(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadRevertParams
	if err := decodeParams(params, stringMembers(map[string]*string{"threadId": &p.ThreadID, "beforeTurnId": &p.BeforeTurnID})); err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	if err := thread.Revert(p.BeforeTurnID); err != nil {
		return nil, hostError(err, p.ThreadID, p.BeforeTurnID)
	}
	info, _ := s.info(p.ThreadID)
	return encode(ThreadReadResponse{Thread: s.threadObject(info, threadStatus(thread), nil)})
}

func (s *server) threadFork(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadForkParams
	if err := decodeParams(params, stringMembers(map[string]*string{"threadId": &p.ThreadID, "lastTurnId": &p.LastTurnID})); err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	fork, err := thread.Fork(ctx, p.LastTurnID)
	if err != nil {
		return nil, hostError(err, p.ThreadID, p.LastTurnID)
	}
	return s.announce(ctx, fork)
}

func (s *server) threadTree(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return nil, err
	}
	thread, err := s.open(ctx, id)
	if err != nil {
		return nil, err
	}
	nodes, err := thread.Tree()
	if err != nil {
		return nil, hostError(err, id, "")
	}
	response := ThreadTreeResponse{Turns: make([]TreeTurn, 0, len(nodes))}
	for _, node := range nodes {
		response.Turns = append(response.Turns, TreeTurn{TurnID: node.TurnID, ParentTurnID: node.ParentTurnID, Preview: node.Preview, Active: node.Active})
	}
	return encode(response)
}

func (s *server) leafSet(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p ThreadLeafSetParams
	if err := decodeParams(params, stringMembers(map[string]*string{"threadId": &p.ThreadID, "turnId": &p.TurnID})); err != nil {
		return nil, err
	}
	thread, err := s.watched(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	if err := thread.SetLeaf(p.TurnID); err != nil {
		return nil, hostError(err, p.ThreadID, p.TurnID)
	}
	return emptyObject(), nil
}
