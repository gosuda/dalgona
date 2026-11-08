package sandbox0

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	sdk "github.com/sandbox0-ai/sdk-go"

	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type workspaceServer struct {
	t           *testing.T
	mu          sync.Mutex
	claimBody   string
	contextBody string
	deleted     bool
}

func (s *workspaceServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		s.t.Errorf("read request: %v", err)
		http.Error(w, "read request", http.StatusBadRequest)
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	switch r.Method + " " + r.URL.Path {
	case "POST /api/v1/sandboxes":
		s.claimBody = string(body)
		writeTestJSON(s.t, w, http.StatusCreated, `{"success":true,"data":{"sandbox_id":"box-1","status":"running","runtime_id":"runtime-1","template":"broken"}}`)
	case "POST /api/v1/sandboxes/box-1/contexts":
		s.contextBody = string(body)
		writeTestJSON(s.t, w, http.StatusCreated, `{"success":true,"data":{"id":"context-1","type":"cmd","running":false,"paused":false,"created_at":"2026-11-08T00:00:00Z","exit_code":1,"state":"done"}}`)
	case "DELETE /api/v1/sandboxes/box-1":
		s.deleted = true
		writeTestJSON(s.t, w, http.StatusAccepted, `{"success":true,"data":{"message":"deleted"}}`)
	default:
		http.Error(w, "unexpected route", http.StatusNotFound)
	}
}

func (s *workspaceServer) snapshot() (string, string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.claimBody, s.contextBody, s.deleted
}

func TestCreateWorkspaceFailureUsesExactErrorAndDeletesBox(t *testing.T) {
	handler := &workspaceServer{t: t}
	server := httptest.NewServer(handler)
	defer server.Close()
	client, err := sdk.NewClient(sdk.WithBaseURL(server.URL), sdk.WithToken("test-token"))
	requireSandbox0(t, err)
	service := New(client, nil, nil)
	id, err := service.Create(t.Context(), sandboxsvc.Spec{Template: "broken"})
	if err == nil {
		t.Fatal("Create succeeded with a failed workspace command")
	}
	if id != "" {
		t.Fatalf("Create id = %q, want empty", id)
	}
	wantError := "runner: sandbox0 template broken cannot create /workspace; use a template whose user can write it"
	if err.Error() != wantError {
		t.Fatalf("Create error = %q, want %q", err, wantError)
	}
	claimBody, contextBody, deleted := handler.snapshot()
	if !deleted {
		t.Fatal("failed box was not deleted")
	}
	assertClaimBody(t, claimBody)
	if !strings.Contains(contextBody, `"command":["sh","-c"`) || !strings.Contains(contextBody, "sudo -n mkdir -p /workspace") {
		t.Fatalf("workspace command body = %s", contextBody)
	}
}

func assertClaimBody(t *testing.T, body string) {
	t.Helper()
	for _, fragment := range []string{`"ttl":1800`, `"hard_ttl":0`, `"auto_resume":false`, `"template":"broken"`} {
		if !strings.Contains(body, fragment) {
			t.Errorf("claim body %s does not contain %s", body, fragment)
		}
	}
}

func writeTestJSON(t *testing.T, w http.ResponseWriter, status int, body string) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if _, err := io.WriteString(w, body); err != nil {
		t.Errorf("write response: %v", err)
	}
}

func requireSandbox0(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}
