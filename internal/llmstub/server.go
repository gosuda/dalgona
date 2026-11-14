package llmstub

import (
	"bytes"
	"context"
	"crypto/subtle"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"

	json "encoding/json/v2"
)

const (
	chatPath      = "/v1/chat/completions"
	modelsPath    = "/v1/models"
	maxTurnPrompt = 80
)

type Options struct {
	Key    string
	Record io.Writer
	Log    io.Writer
}

type handler struct {
	script Script
	opts   Options
	mu     sync.Mutex
	logMu  sync.Mutex
	spent  map[string]bool
	seq    int
}

type lastMessage struct {
	Role       string
	Text       string
	ToolCallID string
}

type requestView struct {
	Last  lastMessage
	Texts []string
}

type requestMessage struct {
	Role       string         `json:"role"`
	Content    jsontext.Value `json:"content,omitzero"`
	ToolCallID string         `json:"tool_call_id,omitzero"`
}

type requestBody struct {
	Messages []requestMessage `json:"messages"`
}

type contentPart struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

type record struct {
	Seq  int            `json:"seq"`
	Path string         `json:"path"`
	Turn string         `json:"turn"`
	Body jsontext.Value `json:"body"`
}

type modelsBody struct {
	Object string      `json:"object"`
	Data   []modelWire `json:"data"`
}

type modelWire struct {
	ID            string        `json:"id"`
	ContextLength int           `json:"context_length,omitzero"`
	Architecture  *architecture `json:"architecture,omitzero"`
}

type architecture struct {
	InputModalities []string `json:"input_modalities"`
}

func NewHandler(script Script, opts Options) http.Handler {
	h := &handler{script: script, opts: opts, spent: make(map[string]bool)}
	mux := http.NewServeMux()
	mux.HandleFunc("POST "+chatPath, func(w http.ResponseWriter, r *http.Request) { h.authorized(w, r, h.chat) })
	mux.HandleFunc("GET "+modelsPath, func(w http.ResponseWriter, r *http.Request) { h.authorized(w, r, h.models) })
	return mux
}

func (h *handler) authorized(w http.ResponseWriter, r *http.Request, next http.HandlerFunc) {
	want := []byte("Bearer " + h.opts.Key)
	if subtle.ConstantTimeCompare([]byte(r.Header.Get("Authorization")), want) != 1 {
		h.writeError(w, http.StatusUnauthorized, "llmstub_auth", "bad bearer token")
		return
	}
	next(w, r)
}

func (h *handler) chat(w http.ResponseWriter, r *http.Request) {
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		h.writeError(w, http.StatusBadRequest, "llmstub_request", err.Error())
		return
	}
	view, err := decodeRequest(raw)
	if err != nil {
		h.writeError(w, http.StatusBadRequest, "llmstub_request", err.Error())
		return
	}
	turn, matched := h.pick(view)
	name := "unmatched"
	if matched {
		name = turn.ID
	}
	if recErr := h.record(raw, name); recErr != nil {
		h.writeError(w, http.StatusInternalServerError, "llmstub_record", recErr.Error())
		return
	}
	if !matched {
		h.writeError(w, http.StatusBadRequest, "llmstub_unmatched", "no scripted turn matches: "+clip(view.Last.Text, maxTurnPrompt))
		return
	}
	if turn.Status != 0 {
		h.writeStatus(w, turn)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	if streamErr := streamTurn(r.Context(), w, turn); streamErr != nil {
		h.logf("llmstub: %s stream: %v\n", turn.ID, streamErr)
	}
}

func (h *handler) models(w http.ResponseWriter, _ *http.Request) {
	body := modelsBody{Object: "list", Data: make([]modelWire, 0, len(h.script.Models))}
	for _, m := range h.script.Models {
		entry := modelWire{ID: m.ID, ContextLength: m.ContextLength}
		if len(m.InputModalities) > 0 {
			entry.Architecture = &architecture{InputModalities: m.InputModalities}
		}
		body.Data = append(body.Data, entry)
	}
	data, err := json.Marshal(body)
	if err != nil {
		h.writeError(w, http.StatusInternalServerError, "llmstub_models", err.Error())
		return
	}
	w.Header().Set("Content-Type", "application/json")
	h.write(w, data)
}

func (h *handler) pick(v requestView) (Turn, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, turn := range h.script.Turns {
		if h.spent[turn.ID] || !matches(turn.When, v) {
			continue
		}
		if turn.Once {
			h.spent[turn.ID] = true
		}
		return turn, true
	}
	return Turn{}, false
}

func matches(when When, v requestView) bool {
	switch {
	case when.User != "":
		return v.Last.Role == "user" && strings.Contains(v.Last.Text, when.User)
	case when.ToolResult != "":
		return v.Last.Role == "tool" && v.Last.ToolCallID == when.ToolResult
	default:
		for _, text := range v.Texts {
			if strings.Contains(text, when.AnyText) {
				return true
			}
		}
		return false
	}
}

func (h *handler) record(raw []byte, turn string) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.seq++
	if h.opts.Record != nil {
		body := jsontext.Value(bytes.Clone(raw))
		if err := body.Compact(); err != nil {
			return err
		}
		line, err := json.Marshal(record{Seq: h.seq, Path: chatPath, Turn: turn, Body: body})
		if err != nil {
			return err
		}
		if _, err = h.opts.Record.Write(append(line, '\n')); err != nil {
			return err
		}
	}
	h.logf("llmstub: %d %s %s\n", h.seq, turn, chatPath)
	return nil
}

func (h *handler) logf(format string, args ...any) {
	if h.opts.Log == nil {
		return
	}
	h.logMu.Lock()
	defer h.logMu.Unlock()
	_, _ = fmt.Fprintf(h.opts.Log, format, args...)
}

func (h *handler) write(w http.ResponseWriter, data []byte) {
	if _, err := w.Write(data); err != nil {
		h.logf("llmstub: write response: %v\n", err)
	}
}

func decodeRequest(raw []byte) (requestView, error) {
	var body requestBody
	if err := json.Unmarshal(raw, &body); err != nil {
		return requestView{}, fmt.Errorf("request body: %w", err)
	}
	var view requestView
	for _, message := range body.Messages {
		text, err := contentText(message.Content)
		if err != nil {
			return requestView{}, err
		}
		view.Texts = append(view.Texts, text)
		view.Last = lastMessage{Role: message.Role, Text: text, ToolCallID: message.ToolCallID}
	}
	return view, nil
}

func contentText(content jsontext.Value) (string, error) {
	if content.Kind() == '"' {
		var text string
		err := json.Unmarshal(content, &text)
		return text, err
	}
	if content.Kind() == '[' {
		return partsText(content)
	}
	return "", nil
}

func partsText(content jsontext.Value) (string, error) {
	var parts []contentPart
	if err := json.Unmarshal(content, &parts); err != nil {
		return "", err
	}
	var text strings.Builder
	for _, part := range parts {
		if part.Type == "text" {
			text.WriteString(part.Text)
		}
	}
	return text.String(), nil
}

func streamTurn(ctx context.Context, w http.ResponseWriter, turn Turn) error {
	rc := http.NewResponseController(w)
	index := 0
	for _, step := range turn.Steps {
		err := writeStep(ctx, w, rc, step, &index)
		if errors.Is(err, errDrop) {
			return nil
		}
		if err != nil {
			return err
		}
	}
	return nil
}

func (h *handler) writeStatus(w http.ResponseWriter, turn Turn) {
	if turn.RetryAfter != "" {
		w.Header().Set("Retry-After", turn.RetryAfter)
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(turn.Status)
	h.write(w, turn.Body)
}

func (h *handler) writeError(w http.ResponseWriter, status int, code, message string) {
	body, err := json.Marshal(errorEnvelope{Error: ErrorFrame{Code: code, Message: message}})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	h.write(w, body)
}

func clip(text string, n int) string {
	runes := []rune(text)
	if len(runes) > n {
		runes = runes[:n]
	}
	return string(runes)
}
