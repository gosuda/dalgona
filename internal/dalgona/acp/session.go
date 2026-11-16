package acp

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	mcpNotice       = "dalgona ignores the MCP servers the editor sent; configure them in config.toml"
	modelRefusal    = "session model is not supported; set model in config.toml"
	replayPageLimit = 100
	stopEndTurn     = "end_turn"
	stopCancelled   = "cancelled"
)

var errDaemonLost = errors.New("the daemon connection closed")

type session struct {
	id          string
	mu          sync.Mutex
	turn        *activeTurn
	streamed    map[string]struct{}
	cancelAsked bool
}

type activeTurn struct {
	id    string
	done  chan struct{}
	over  bool
	final rpc.Turn
}

func newSession(id string) *session {
	return &session{id: id, streamed: make(map[string]struct{})}
}

func (s *session) begin() (*activeTurn, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.turn != nil {
		return nil, false
	}
	s.turn = &activeTurn{done: make(chan struct{})}
	s.cancelAsked = false
	return s.turn, true
}

func (s *session) end() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.turn = nil
	s.cancelAsked = false
	clear(s.streamed)
}

func (s *session) adopt(turnID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.turn == nil {
		return false
	}
	s.turn.id = cmp.Or(s.turn.id, turnID)
	return s.cancelAsked && !s.turn.over
}

func (s *session) finish(turn rpc.Turn) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.turn
	if t == nil || t.over || (t.id != "" && t.id != turn.ID) {
		return
	}
	t.id, t.final, t.over = turn.ID, turn, true
	close(t.done)
}

func (s *session) requestCancel() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cancelAsked = true
	if s.turn == nil || s.turn.over {
		return ""
	}
	return s.turn.id
}

func (s *session) markStreamed(itemID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.streamed[itemID] = struct{}{}
}

func (s *session) wasStreamed(itemID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	_, ok := s.streamed[itemID]
	return ok
}

func (a *adapter) addSession(id string) *session {
	a.mu.Lock()
	defer a.mu.Unlock()
	if s, ok := a.sessions[id]; ok {
		return s
	}
	s := newSession(id)
	a.sessions[id] = s
	return s
}

func (a *adapter) lookup(id string) *session {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.sessions[id]
}

type (
	newSessionParams struct {
		Cwd        string           `json:"cwd"`
		MCPServers []jsontext.Value `json:"mcpServers"`
	}
	sessionResult struct {
		SessionID string `json:"sessionId"`
	}
	loadSessionParams struct {
		SessionID string `json:"sessionId"`
	}
	listParams struct {
		Cwd    string `json:"cwd"`
		Cursor string `json:"cursor"`
	}
	sessionInfo struct {
		SessionID string `json:"sessionId"`
		Cwd       string `json:"cwd"`
		Title     string `json:"title,omitzero"`
		UpdatedAt string `json:"updatedAt,omitzero"`
	}
	listResult struct {
		Sessions   []sessionInfo `json:"sessions"`
		NextCursor string        `json:"nextCursor,omitzero"`
	}
	promptParams struct {
		SessionID string         `json:"sessionId"`
		Prompt    []contentBlock `json:"prompt"`
	}
	contentBlock struct {
		Type     string `json:"type"`
		Text     string `json:"text"`
		Data     string `json:"data"`
		MimeType string `json:"mimeType"`
	}
	promptResult struct {
		StopReason string `json:"stopReason"`
	}
	cancelParams struct {
		SessionID string `json:"sessionId"`
	}
	configOptionParams struct {
		ConfigID string `json:"configId"`
	}
)

func (a *adapter) newSession(ctx context.Context, params jsontext.Value) (reply, error) {
	p, err := decode[newSessionParams](params)
	if err != nil {
		return reply{}, err
	}
	if p.Cwd == "" {
		return reply{}, rpcError(codeInvalidParams, "missing cwd")
	}
	d, err := a.conn()
	if err != nil {
		return reply{}, err
	}
	var started rpc.ThreadStartResponse
	if err = d.client.ThreadStart(ctx, rpc.ThreadStartParams{Cwd: p.Cwd}, &started); err != nil {
		return reply{}, coded(err)
	}
	if started.Thread.ID == "" {
		return reply{}, rpcError(codeInternal, "the daemon returned no thread id")
	}
	s := a.addSession(started.Thread.ID)
	res := reply{result: sessionResult{SessionID: s.id}}
	if len(p.MCPServers) > 0 {
		res.after = func() { a.chunk(s.id, kindAgentChunk, mcpNotice) }
	}
	return res, nil
}

func (a *adapter) loadSession(ctx context.Context, params jsontext.Value) (reply, error) {
	p, err := decode[loadSessionParams](params)
	if err != nil {
		return reply{}, err
	}
	if p.SessionID == "" {
		return reply{}, rpcError(codeInvalidParams, "missing sessionId")
	}
	d, err := a.conn()
	if err != nil {
		return reply{}, err
	}
	resume := rpc.ThreadResumeParams{ThreadID: p.SessionID, ExcludeTurns: true}
	if err = d.client.Call(ctx, "thread/resume", resume, nil); err != nil {
		return reply{}, coded(err)
	}
	a.addSession(p.SessionID)
	if err = a.replay(ctx, d, p.SessionID); err != nil {
		return reply{}, coded(err)
	}
	return reply{}, nil
}

func (a *adapter) replay(ctx context.Context, d *daemonConn, sessionID string) error {
	cursor := ""
	for {
		var page rpc.ThreadTurnsListResponse
		params := rpc.ThreadTurnsListParams{ThreadID: sessionID, Cursor: cursor, Limit: replayPageLimit, SortDirection: "asc", ItemsView: "full"}
		if err := d.client.Call(ctx, "thread/turns/list", params, &page); err != nil {
			return err
		}
		a.replayTurns(sessionID, page.Data)
		if page.NextCursor == "" || page.NextCursor == cursor {
			return nil
		}
		cursor = page.NextCursor
	}
}

func (a *adapter) replayTurns(sessionID string, turns []rpc.Turn) {
	for _, turn := range turns {
		for _, it := range turn.Items {
			if kind := replayKind(it.Type); kind != "" && it.Text != "" {
				a.chunk(sessionID, kind, it.Text)
			}
		}
	}
}

func replayKind(itemType string) string {
	switch itemType {
	case itemUser:
		return kindUserChunk
	case itemAgent:
		return kindAgentChunk
	}
	return ""
}

func (a *adapter) listSessions(ctx context.Context, params jsontext.Value) (reply, error) {
	p, err := decode[listParams](params)
	if err != nil {
		return reply{}, err
	}
	d, err := a.conn()
	if err != nil {
		return reply{}, err
	}
	var list rpc.ThreadListResponse
	if err = d.client.Call(ctx, "thread/list", rpc.ThreadListParams{Cursor: p.Cursor}, &list); err != nil {
		return reply{}, coded(err)
	}
	return reply{result: sessionList(list, p.Cwd)}, nil
}

func sessionList(list rpc.ThreadListResponse, cwd string) listResult {
	out := listResult{Sessions: make([]sessionInfo, 0, len(list.Data)), NextCursor: list.NextCursor}
	for _, t := range list.Data {
		if cwd != "" && t.Cwd != cwd {
			continue
		}
		out.Sessions = append(out.Sessions, sessionInfo{SessionID: t.ID, Cwd: t.Cwd, Title: cmp.Or(t.Name, t.Preview), UpdatedAt: isoTime(t.UpdatedAt)})
	}
	return out
}

func isoTime(seconds int64) string {
	if seconds == 0 {
		return ""
	}
	return time.Unix(seconds, 0).UTC().Format(time.RFC3339)
}

func (a *adapter) preparePrompt(params jsontext.Value) (*session, []rpc.InputItem, error) {
	p, err := decode[promptParams](params)
	if err != nil {
		return nil, nil, err
	}
	if _, err = a.conn(); err != nil {
		return nil, nil, err
	}
	s := a.lookup(p.SessionID)
	if s == nil {
		return nil, nil, rpcError(codeInvalidParams, "unknown session "+p.SessionID)
	}
	input, err := promptInput(p.Prompt)
	return s, input, err
}

func (a *adapter) prompt(ctx context.Context, params jsontext.Value) (reply, error) {
	s, input, err := a.preparePrompt(params)
	if err != nil {
		return reply{}, err
	}
	d, err := a.conn()
	if err != nil {
		return reply{}, err
	}
	turn, ok := s.begin()
	if !ok {
		return reply{}, rpcError(codeInvalidRequest, "session "+s.id+" already runs a prompt")
	}
	defer s.end()
	final, err := a.runTurn(ctx, d, s, turn, input)
	if err != nil {
		return reply{}, coded(err)
	}
	stop, err := stopReason(final)
	if err != nil {
		return reply{}, err
	}
	return reply{result: promptResult{StopReason: stop}}, nil
}

func (a *adapter) runTurn(ctx context.Context, d *daemonConn, s *session, t *activeTurn, input []rpc.InputItem) (rpc.Turn, error) {
	var started rpc.TurnStartResponse
	if err := d.client.TurnStart(ctx, rpc.TurnStartParams{ThreadID: s.id, Input: input}, &started); err != nil {
		return rpc.Turn{}, err
	}
	if s.adopt(started.Turn.ID) {
		a.interrupt(ctx, d, s.id, started.Turn.ID)
	}
	select {
	case <-t.done:
		return t.final, nil
	case <-ctx.Done():
		return rpc.Turn{}, ctx.Err()
	case <-d.lost:
		return rpc.Turn{}, errDaemonLost
	}
}

func (a *adapter) interrupt(ctx context.Context, d *daemonConn, threadID, turnID string) {
	params := rpc.TurnInterruptParams{ThreadID: threadID, TurnID: turnID}
	if err := d.client.Call(ctx, "turn/interrupt", params, nil); err != nil {
		a.log.Debug("acp: interrupt turn", "thread", threadID, "turn", turnID, "error", err)
	}
}

func stopReason(turn rpc.Turn) (string, error) {
	switch turn.Status {
	case "completed":
		return stopEndTurn, nil
	case "interrupted":
		return stopCancelled, nil
	case "failed":
		message := "turn failed"
		if turn.Error != nil {
			message = cmp.Or(turn.Error.Message, message)
		}
		return "", rpcError(codeInternal, message)
	}
	return "", rpcError(codeInternal, "unexpected turn status "+turn.Status)
}

func promptInput(blocks []contentBlock) ([]rpc.InputItem, error) {
	items := make([]rpc.InputItem, 0, len(blocks))
	for _, block := range blocks {
		item, err := inputItem(block)
		if err != nil {
			return nil, err
		}
		items = append(items, item)
	}
	return items, nil
}

func inputItem(block contentBlock) (rpc.InputItem, error) {
	switch block.Type {
	case "text":
		return rpc.InputItem{Type: "text", Text: block.Text}, nil
	case "image":
		return rpc.InputItem{Type: "image", URL: "data:" + block.MimeType + ";base64," + block.Data}, nil
	}
	return rpc.InputItem{}, rpcError(codeInvalidParams, "unsupported prompt block "+block.Type)
}

func (a *adapter) cancel(ctx context.Context, params jsontext.Value) (reply, error) {
	p, err := decode[cancelParams](params)
	if err != nil {
		return reply{}, err
	}
	if _, err = a.conn(); err != nil {
		return reply{}, err
	}
	s := a.lookup(p.SessionID)
	if s == nil {
		return reply{}, nil
	}
	d, err := a.conn()
	if err != nil {
		return reply{}, err
	}
	if id := s.requestCancel(); id != "" {
		a.interrupt(ctx, d, s.id, id)
	}
	return reply{}, nil
}

func (a *adapter) setConfigOption(_ context.Context, params jsontext.Value) (reply, error) {
	if _, err := a.conn(); err != nil {
		return reply{}, err
	}
	p, err := decode[configOptionParams](params)
	if err != nil {
		return reply{}, err
	}
	if p.ConfigID == "model" {
		return reply{}, rpcError(codeMethodNotFound, modelRefusal)
	}
	return reply{}, rpcError(codeMethodNotFound, "Method not found")
}
