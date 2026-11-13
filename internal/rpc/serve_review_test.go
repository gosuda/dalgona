package rpc

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

type bodyLog struct {
	mu     sync.Mutex
	bodies []string
}

func (l *bodyLog) last() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.bodies) == 0 {
		return ""
	}
	return l.bodies[len(l.bodies)-1]
}

func (l *bodyLog) count() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.bodies)
}

func recordBodies(t *testing.T, r *rig) *bodyLog {
	t.Helper()
	log := &bodyLog{}
	f := &fakeLLM{}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", func(w http.ResponseWriter, req *http.Request) {
		body, err := io.ReadAll(req.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		log.mu.Lock()
		log.bodies = append(log.bodies, string(body))
		log.mu.Unlock()
		req.Body = io.NopCloser(bytes.NewReader(body))
		f.respond(w, req)
	})
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	r.llm = f
	return log
}

func TestReviewStartRunsAReviewTurn(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	bodies := recordBodies(t, r)
	r.llm.push(llmReply{body: sseText("reviewed")})
	c := r.connect()
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd}, &started)
	c.await("thread/started", nil)
	threadID := started.Thread.ID

	var response ReviewStartResponse
	c.ok("review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "uncommittedChanges"}}, &response)
	expect(t, response.ReviewThreadID == threadID && response.Turn.ID != "" && response.Turn.Status == statusInProgress, "review/start", response)
	log := readTurn(t, c.collect("turn/completed"))
	expect(t, log.status == statusCompleted, "turn status", log.status)
	expect(t, strings.Contains(bodies.last(), "Review the uncommitted changes in this workspace: staged, unstaged, and untracked files."),
		"review prompt in the model request", bodies.last())

	rejected := bodies.count()
	c.fails("review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "baseBranch", "branch": "-x"}}, -32602, "invalid branch -x")
	c.fails("review/start", map[string]any{"threadId": threadID, "delivery": "detached", "target": map[string]any{"type": "uncommittedChanges"}}, -32602, "unsupported delivery detached")
	c.fails("review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "commit", "sha": "zz"}}, -32602, "invalid sha zz")
	c.fails("review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "custom", "instructions": " "}}, -32602, "instructions must not be empty")
	c.fails("review/start", map[string]any{"threadId": threadID, "delivery": "", "target": map[string]any{"type": "uncommittedChanges"}}, -32602, "unsupported delivery ")
	c.fails("review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "bogus"}}, -32602, "unsupported review target bogus")
	expect(t, bodies.count() == rejected, "rejected review/start sent a model request", bodies.count()-rejected)
}
