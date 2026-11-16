package acp

import (
	"bytes"
	"cmp"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	kindUserChunk    = "user_message_chunk"
	kindAgentChunk   = "agent_message_chunk"
	kindThoughtChunk = "agent_thought_chunk"

	itemUser       = "userMessage"
	itemAgent      = "agentMessage"
	itemReasoning  = "reasoning"
	itemCommand    = "commandExecution"
	itemFileChange = "fileChange"
	itemMCP        = "mcpToolCall"
	itemDynamic    = "dynamicToolCall"

	toolStarted  = "in_progress"
	toolFinished = "completed"
	toolFailed   = "failed"
)

type (
	sessionNotification struct {
		SessionID string `json:"sessionId"`
		Update    any    `json:"update"`
	}
	textBlock struct {
		Type string `json:"type"`
		Text string `json:"text"`
	}
	toolContent struct {
		Type    string    `json:"type"`
		Content textBlock `json:"content"`
	}
	location struct {
		Path string `json:"path"`
	}
	toolCallNote struct {
		SessionUpdate string        `json:"sessionUpdate,omitzero"`
		ToolCallID    string        `json:"toolCallId"`
		Title         string        `json:"title,omitzero"`
		Kind          string        `json:"kind,omitzero"`
		Status        string        `json:"status,omitzero"`
		Content       []toolContent `json:"content,omitzero"`
		Locations     []location    `json:"locations,omitzero"`
	}
	planEntry struct {
		Content  string `json:"content"`
		Priority string `json:"priority"`
		Status   string `json:"status"`
	}
	planNote struct {
		SessionUpdate string      `json:"sessionUpdate"`
		Entries       []planEntry `json:"entries"`
	}
	cost struct {
		Amount   float64 `json:"amount"`
		Currency string  `json:"currency"`
	}
	usageNote struct {
		SessionUpdate string `json:"sessionUpdate"`
		Used          int    `json:"used"`
		Size          int    `json:"size"`
		Cost          *cost  `json:"cost,omitzero"`
	}
)

func (a *adapter) Notification(method string, params jsontext.Value) {
	switch method {
	case "item/agentMessage/delta":
		a.delta(params, kindAgentChunk)
	case "item/reasoning/textDelta":
		a.delta(params, kindThoughtChunk)
	case "item/started":
		a.itemStarted(params)
	case "item/completed":
		a.itemCompleted(params)
	case "turn/plan/updated":
		a.planUpdated(params)
	case "thread/tokenUsage/updated":
		a.usageUpdated(params)
	case "turn/started":
		a.turnStarted(params)
	case "turn/completed":
		a.turnCompleted(params)
	}
}

func decodeNote[T any](a *adapter, method string, params jsontext.Value) (T, bool) {
	var note T
	if err := json.Unmarshal(params, &note); err != nil {
		a.log.Debug("acp: decode notification", "method", method, "error", err)
		return note, false
	}
	return note, true
}

func (a *adapter) notifyUpdate(raw jsontext.Value, err error) {
	if err == nil {
		err = a.peer.notify("session/update", raw)
	}
	a.logSend("send session update", err)
}

func (a *adapter) update(sessionID string, update any) {
	raw, err := json.Marshal(sessionNotification{SessionID: sessionID, Update: update})
	a.notifyUpdate(raw, err)
}

func (a *adapter) chunk(sessionID, kind, text string) {
	raw, err := chunkParams(sessionID, kind, text)
	a.notifyUpdate(raw, err)
}

func chunkParams(sessionID, kind, text string) (jsontext.Value, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf)
	tokens := []jsontext.Token{
		jsontext.BeginObject,
		jsontext.String("sessionId"), jsontext.String(sessionID),
		jsontext.String("update"), jsontext.BeginObject,
		jsontext.String("sessionUpdate"), jsontext.String(kind),
		jsontext.String("content"), jsontext.BeginObject,
		jsontext.String("type"), jsontext.String("text"),
		jsontext.String("text"), jsontext.String(text),
		jsontext.EndObject, jsontext.EndObject, jsontext.EndObject,
	}
	for _, token := range tokens {
		if err := enc.WriteToken(token); err != nil {
			return nil, fmt.Errorf("encode chunk: %w", err)
		}
	}
	return jsontext.Value(bytes.TrimSuffix(buf.Bytes(), []byte{'\n'})), nil
}

type delta struct {
	threadID string
	itemID   string
	text     string
}

func readDelta(params jsontext.Value) (delta, error) {
	var d delta
	err := readMembers(params, func(name string, dec *jsontext.Decoder) error {
		var target *string
		switch name {
		case "threadId":
			target = &d.threadID
		case "itemId":
			target = &d.itemID
		case "delta":
			target = &d.text
		default:
			return dec.SkipValue()
		}
		value, err := readString(dec)
		*target = value
		return err
	})
	return d, err
}

func (a *adapter) delta(params jsontext.Value, kind string) {
	d, err := readDelta(params)
	if err != nil {
		a.log.Debug("acp: decode delta", "error", err)
		return
	}
	s := a.lookup(d.threadID)
	if s == nil || d.text == "" {
		return
	}
	s.markStreamed(d.itemID)
	a.chunk(s.id, kind, d.text)
}

func toolKind(itemType string) (string, bool) {
	switch itemType {
	case itemCommand:
		return "execute", true
	case itemFileChange:
		return "edit", true
	case itemMCP, itemDynamic:
		return "other", true
	}
	return "", false
}

func (a *adapter) itemStarted(params jsontext.Value) {
	note, ok := decodeNote[rpc.ItemStartedNotification](a, "item/started", params)
	if !ok {
		return
	}
	s := a.lookup(note.ThreadID)
	kind, tool := toolKind(note.Item.Type)
	if s == nil || !tool {
		return
	}
	it := note.Item
	a.update(s.id, toolCallNote{
		SessionUpdate: "tool_call", ToolCallID: it.ID, Title: itemTitle(it), Kind: kind, Status: toolStarted, Locations: itemLocations(it),
	})
}

func (a *adapter) itemCompleted(params jsontext.Value) {
	note, ok := decodeNote[rpc.ItemCompletedNotification](a, "item/completed", params)
	if !ok {
		return
	}
	s := a.lookup(note.ThreadID)
	if s == nil {
		return
	}
	if _, tool := toolKind(note.Item.Type); tool {
		a.update(s.id, toolDone(note.Item))
		return
	}
	a.unstreamed(s, note.Item)
}

func (a *adapter) unstreamed(s *session, it rpc.Item) {
	kind := ""
	switch it.Type {
	case itemAgent:
		kind = kindAgentChunk
	case itemReasoning:
		kind = kindThoughtChunk
	}
	if kind == "" || it.Text == "" || s.wasStreamed(it.ID) {
		return
	}
	a.chunk(s.id, kind, it.Text)
}

func toolDone(it rpc.Item) toolCallNote {
	status := toolFailed
	if it.Status == "completed" {
		status = toolFinished
	}
	return toolCallNote{
		SessionUpdate: "tool_call_update", ToolCallID: it.ID, Title: itemTitle(it), Status: status,
		Content: itemContent(it), Locations: itemLocations(it),
	}
}

func itemTitle(it rpc.Item) string {
	switch it.Type {
	case itemCommand:
		return cmp.Or(it.Command, "Run command")
	case itemFileChange:
		return editTitle(it.Changes)
	case itemMCP:
		if it.Server != "" {
			return it.Server + "." + it.Tool
		}
	}
	return cmp.Or(it.Tool, it.ID)
}

func editTitle(changes []rpc.FileChange) string {
	if len(changes) == 0 {
		return "Edit files"
	}
	paths := make([]string, 0, len(changes))
	for _, change := range changes {
		paths = append(paths, change.Path)
	}
	return "Edit " + strings.Join(paths, ", ")
}

func itemLocations(it rpc.Item) []location {
	var out []location
	for _, change := range it.Changes {
		out = append(out, location{Path: change.Path})
	}
	return out
}

func itemContent(it rpc.Item) []toolContent {
	var out []toolContent
	if it.Type != itemFileChange {
		return appendText(out, it.Output)
	}
	for _, change := range it.Changes {
		out = appendText(out, change.Diff)
	}
	return out
}

func appendText(out []toolContent, text string) []toolContent {
	if text == "" {
		return out
	}
	return append(out, textContent(text))
}

func textContent(text string) toolContent {
	return toolContent{Type: "content", Content: textBlock{Type: "text", Text: text}}
}

func (a *adapter) planUpdated(params jsontext.Value) {
	note, ok := decodeNote[rpc.TurnPlanUpdatedNotification](a, "turn/plan/updated", params)
	if !ok {
		return
	}
	s := a.lookup(note.ThreadID)
	if s == nil {
		return
	}
	entries := make([]planEntry, 0, len(note.Plan))
	for _, step := range note.Plan {
		entries = append(entries, planEntry{Content: step.Step, Priority: "medium", Status: planStatus(step.Status)})
	}
	a.update(s.id, planNote{SessionUpdate: "plan", Entries: entries})
}

func planStatus(status string) string {
	switch status {
	case "inProgress":
		return "in_progress"
	case "completed":
		return "completed"
	}
	return "pending"
}

func (a *adapter) usageUpdated(params jsontext.Value) {
	note, ok := decodeNote[rpc.ThreadTokenUsageUpdatedNotification](a, "thread/tokenUsage/updated", params)
	if !ok {
		return
	}
	s := a.lookup(note.ThreadID)
	usage := note.TokenUsage
	if s == nil || usage.ModelContextWindow <= 0 {
		return
	}
	update := usageNote{SessionUpdate: "usage_update", Used: max(usage.Last.TotalTokens, 0), Size: usage.ModelContextWindow}
	if usage.CostUsdMicros != nil {
		update.Cost = &cost{Amount: float64(*usage.CostUsdMicros) / 1e6, Currency: "USD"}
	}
	a.update(s.id, update)
}

func (a *adapter) turnStarted(params jsontext.Value) {
	note, ok := decodeNote[rpc.TurnStartedNotification](a, "turn/started", params)
	if !ok {
		return
	}
	if s := a.lookup(note.ThreadID); s != nil {
		s.adopt(note.Turn.ID)
	}
}

func (a *adapter) turnCompleted(params jsontext.Value) {
	note, ok := decodeNote[rpc.TurnCompletedNotification](a, "turn/completed", params)
	if !ok {
		return
	}
	if s := a.lookup(note.ThreadID); s != nil {
		s.finish(note.Turn)
	}
}
