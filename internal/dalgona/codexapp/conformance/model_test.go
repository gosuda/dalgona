package conformance

import (
	"bytes"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

const (
	approvalMarker   = "NEED_COMMAND_APPROVAL"
	approvalCommand  = "printf 'conformance-%s' approved"
	approvalOutput   = "conformance-approved"
	replyText        = "Conformance reply from the fake model."
	afterApprovalTxt = "The approved command finished."
	notRunTxt        = "The command did not run."
)

type fakeModel struct {
	server *httptest.Server
}

func newFakeModel(t *testing.T) *fakeModel {
	t.Helper()
	m := &fakeModel{}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", m.respond)
	mux.HandleFunc("GET /models", listModels)
	m.server = httptest.NewServer(mux)
	t.Cleanup(m.server.Close)
	return m
}

func (m *fakeModel) URL() string { return m.server.URL }

func (m *fakeModel) respond(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, responseFor(body))
}

func responseFor(body []byte) string {
	switch {
	case bytes.Contains(body, []byte(`"function_call_output"`)) && bytes.Contains(body, []byte(approvalOutput)):
		return textStream(afterApprovalTxt)
	case bytes.Contains(body, []byte(`"function_call_output"`)):
		return textStream(notRunTxt)
	case bytes.Contains(body, []byte(approvalMarker)):
		return execStream(approvalCommand)
	default:
		return textStream(replyText)
	}
}

func listModels(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
}

func sseEvent(kind, data string) string {
	return "event: " + kind + "\ndata: " + data + "\n\n"
}

func quoted(text string) string {
	value, err := jsontext.AppendQuote(nil, text)
	if err != nil {
		panic("conformance model text must be valid UTF-8: " + err.Error())
	}
	return string(value)
}

func completedEvent() string {
	return sseEvent("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":7}}}`)
}

func textStream(text string) string {
	half := len(text) / 2
	return sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+quoted(text[:half])+`}`) +
		sseEvent("response.output_text.delta", `{"type":"response.output_text.delta","output_index":0,"delta":`+quoted(text[half:])+`}`) +
		completedEvent()
}

func execStream(command string) string {
	args := `{"command":` + quoted(command) + `}`
	added := `{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_conformance","name":"exec","arguments":""}}`
	done := `{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_conformance","name":"exec","arguments":` + quoted(args) + `}}`
	return sseEvent("response.output_item.added", added) + sseEvent("response.output_item.done", done) + completedEvent()
}
