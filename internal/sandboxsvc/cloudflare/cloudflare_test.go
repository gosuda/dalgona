package cloudflare

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/coder/websocket"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type requestLog struct {
	mu      sync.Mutex
	methods []string
	paths   []string
}

func (l *requestLog) add(r *http.Request) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.methods = append(l.methods, r.Method)
	l.paths = append(l.paths, r.URL.Path)
}

func (l *requestLog) snapshot() ([]string, []string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return slices.Clone(l.methods), slices.Clone(l.paths)
}

func newLifecycleServer(t *testing.T) (*httptest.Server, *requestLog) {
	t.Helper()
	log := &requestLog{}
	mux := http.NewServeMux()
	noContent := func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}
	mux.HandleFunc("POST /box/{id}", noContent)
	mux.HandleFunc("DELETE /box/{id}", noContent)
	websocketHandler := func(w http.ResponseWriter, r *http.Request) {
		conn, acceptErr := websocket.Accept(w, r, nil)
		if acceptErr != nil {
			t.Errorf("accept websocket: %v", acceptErr)
			return
		}
		kind, data, readErr := conn.Read(context.Background())
		if readErr == nil {
			if writeErr := conn.Write(context.Background(), kind, data); writeErr != nil {
				t.Errorf("write websocket echo: %v", writeErr)
			}
		} else {
			t.Errorf("read websocket message: %v", readErr)
		}
		if closeErr := conn.CloseNow(); closeErr != nil {
			t.Errorf("close websocket: %v", closeErr)
		}
	}
	mux.HandleFunc("GET /box/{id}", websocketHandler)
	mux.HandleFunc("GET /box/{id}/bridge", websocketHandler)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer token" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		log.add(r)
		mux.ServeHTTP(w, r)
	}))
	t.Cleanup(server.Close)
	return server, log
}

func assertRequests(t *testing.T, log *requestLog, wantMethods, wantPaths []string) {
	t.Helper()
	methods, paths := log.snapshot()
	if !slices.Equal(methods, wantMethods) {
		t.Fatalf("methods %v, want %v", methods, wantMethods)
	}
	if !slices.Equal(paths, wantPaths) {
		t.Fatalf("paths %v, want %v", paths, wantPaths)
	}
}

func TestServiceHTTPLifecycle(t *testing.T) {
	t.Parallel()
	server, log := newLifecycleServer(t)
	service := New(server.URL, "token")
	id, createErr := service.Create(t.Context(), sandboxsvc.Spec{Name: "box-1"})
	if createErr != nil || id != "box-1" {
		t.Fatalf("create: id=%q err=%v", id, createErr)
	}
	attached, attachErr := service.Attach(t.Context(), id)
	if attachErr != nil || attached != id {
		t.Fatalf("attach: id=%q err=%v", attached, attachErr)
	}
	if destroyErr := service.Destroy(t.Context(), id); destroyErr != nil {
		t.Fatalf("destroy: %v", destroyErr)
	}
	assertRequests(t, log,
		[]string{http.MethodPost, http.MethodPost, http.MethodDelete},
		[]string{"/box/box-1", "/box/box-1", "/box/box-1"},
	)
}

func TestServiceConnect(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		endpoint sandboxsvc.Endpoint
		path     string
	}{
		{name: "exec server", endpoint: sandboxsvc.ExecServer, path: "/box/box-1"},
		{name: "bridge", endpoint: sandboxsvc.Bridge, path: "/box/box-1/bridge"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			server, log := newLifecycleServer(t)
			service := New(server.URL, "token")
			conn, connectErr := service.Connect(t.Context(), "box-1", test.endpoint)
			if connectErr != nil {
				t.Fatalf("connect: %v", connectErr)
			}
			if _, writeErr := conn.Write([]byte("hello")); writeErr != nil {
				t.Fatalf("write: %v", writeErr)
			}
			got := make([]byte, 5)
			if _, readErr := io.ReadFull(conn, got); readErr != nil {
				t.Fatalf("read: %v", readErr)
			}
			if string(got) != "hello" {
				t.Fatalf("echo %q", got)
			}
			_ = conn.Close()
			assertRequests(t, log, []string{http.MethodGet}, []string{test.path})
		})
	}
}

func TestServiceUnauthorized(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	t.Cleanup(server.Close)
	service := New(server.URL, "wrong")
	if _, attachErr := service.Attach(t.Context(), "box"); attachErr == nil || !strings.Contains(attachErr.Error(), "401") {
		t.Fatalf("attach error = %v, want 401", attachErr)
	}
	if _, connectErr := service.Connect(t.Context(), "box", sandboxsvc.ExecServer); connectErr == nil || !strings.Contains(connectErr.Error(), "401") {
		t.Fatalf("connect error = %v, want 401", connectErr)
	}
}
