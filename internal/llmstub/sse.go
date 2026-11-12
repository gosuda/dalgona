package llmstub

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"net/http"
	"time"

	json "encoding/json/v2"
)

var (
	errDrop    = errors.New("llmstub: connection dropped by script")
	doneMarker = jsontext.Value("[DONE]")
)

type choice struct {
	Index        int            `json:"index"`
	Delta        map[string]any `json:"delta"`
	FinishReason string         `json:"finish_reason,omitzero"`
}

type envelope struct {
	Choices []choice `json:"choices"`
	Usage   *Usage   `json:"usage,omitzero"`
}

type errorEnvelope struct {
	Error ErrorFrame `json:"error"`
}

func marshal(value any) (jsontext.Value, error) {
	data, err := json.Marshal(value)
	return jsontext.Value(data), err
}

func frame(w http.ResponseWriter, rc *http.ResponseController, payload jsontext.Value) error {
	line := make([]byte, 0, len(payload)+len("data: \n\n"))
	line = append(line, "data: "...)
	line = append(line, payload...)
	line = append(line, "\n\n"...)
	if _, err := w.Write(line); err != nil {
		return err
	}
	return rc.Flush()
}

func writeStep(ctx context.Context, w http.ResponseWriter, rc *http.ResponseController, s Step, index *int) error {
	switch {
	case s.ToolCall != nil:
		return writeTool(w, rc, *s.ToolCall, index)
	case s.Finish != "":
		return writeFinish(w, rc, s)
	case s.ErrorFrame != nil:
		return writeErrorFrame(w, rc, *s.ErrorFrame)
	case s.Drop:
		return errDrop
	case s.Reasoning != "":
		return writeContent(w, rc, "reasoning_content", s.Reasoning)
	case s.Text != "":
		return writeContent(w, rc, "content", s.Text)
	case s.DelayMS > 0:
		return wait(ctx, time.Duration(s.DelayMS)*time.Millisecond)
	default:
		return nil
	}
}

func wait(ctx context.Context, d time.Duration) error {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func writeContent(w http.ResponseWriter, rc *http.ResponseController, key, text string) error {
	payload, err := marshal(envelope{Choices: []choice{{Delta: map[string]any{key: text}}}})
	if err != nil {
		return err
	}
	return frame(w, rc, payload)
}

func writeFinish(w http.ResponseWriter, rc *http.ResponseController, s Step) error {
	payload, err := marshal(envelope{Choices: []choice{{Delta: map[string]any{}, FinishReason: s.Finish}}, Usage: finishUsage(s.Usage)})
	if err != nil {
		return err
	}
	if err = frame(w, rc, payload); err != nil {
		return err
	}
	return frame(w, rc, doneMarker)
}

func writeErrorFrame(w http.ResponseWriter, rc *http.ResponseController, e ErrorFrame) error {
	payload, err := marshal(errorEnvelope{Error: e})
	if err != nil {
		return err
	}
	if err = frame(w, rc, payload); err != nil {
		return err
	}
	return frame(w, rc, doneMarker)
}

func writeTool(w http.ResponseWriter, rc *http.ResponseController, call ToolCall, index *int) error {
	payloads, err := toolFrames(call, *index)
	if err != nil {
		return err
	}
	*index++
	for _, payload := range payloads {
		if err = frame(w, rc, payload); err != nil {
			return err
		}
	}
	return nil
}

func toolFrames(call ToolCall, index int) ([]jsontext.Value, error) {
	pieces := splitChunks(string(call.Arguments), max(call.Chunks, 1))
	payloads := make([]jsontext.Value, 0, len(pieces))
	for i, piece := range pieces {
		function := map[string]any{"arguments": piece}
		entry := map[string]any{"index": index, "function": function}
		if i == 0 {
			entry["id"] = call.ID
			entry["type"] = "function"
			function["name"] = call.Name
		}
		payload, err := marshal(envelope{Choices: []choice{{Delta: map[string]any{"tool_calls": []any{entry}}}}})
		if err != nil {
			return nil, err
		}
		payloads = append(payloads, payload)
	}
	return payloads, nil
}

func splitChunks(text string, chunks int) []string {
	runes := []rune(text)
	n := min(chunks, len(runes))
	if n < 1 {
		return []string{text}
	}
	pieces := make([]string, 0, n)
	for i := range n {
		lo := i * len(runes) / n
		hi := (i + 1) * len(runes) / n
		pieces = append(pieces, string(runes[lo:hi]))
	}
	return pieces
}
