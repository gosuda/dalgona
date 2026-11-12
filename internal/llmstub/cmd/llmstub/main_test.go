package main

import (
	"bytes"
	"context"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"
)

const servingPrefix = "llmstub: serving http://"

type serveWatch struct {
	mu    sync.Mutex
	buf   []byte
	found chan string
	once  sync.Once
}

func (w *serveWatch) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.buf = append(w.buf, p...)
	for {
		i := bytes.IndexByte(w.buf, '\n')
		if i < 0 {
			return len(p), nil
		}
		line := string(w.buf[:i])
		w.buf = w.buf[i+1:]
		if strings.HasPrefix(line, servingPrefix) {
			w.once.Do(func() { w.found <- line })
		}
	}
}

func TestRunRequiresKey(t *testing.T) {
	t.Parallel()
	var stderr bytes.Buffer
	code := run(t.Context(), []string{"-script", "../../testdata/live.json"}, &stderr)
	if code != 2 || !strings.Contains(stderr.String(), "-key") {
		t.Fatalf("exit = %d, stderr = %q", code, stderr.String())
	}
}

func TestRunServesAndStopsOnCancel(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	watch := &serveWatch{found: make(chan string, 1)}
	result := make(chan int, 1)
	go func() {
		result <- run(ctx, []string{"-listen", "127.0.0.1:0", "-script", "../../testdata/live.json", "-key", "dummy"}, watch)
	}()
	addr := awaitAddress(t, watch.found)
	fetchModels(ctx, t, addr)
	cancel()
	awaitExit(t, result)
}

func awaitAddress(t *testing.T, found <-chan string) string {
	t.Helper()
	select {
	case line := <-found:
		return strings.TrimSuffix(strings.TrimPrefix(line, servingPrefix), "/v1")
	case <-time.After(5 * time.Second):
		t.Fatal("the command did not print its address")
		return ""
	}
}

func fetchModels(ctx context.Context, t *testing.T, addr string) {
	t.Helper()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "http://"+addr+"/v1/models", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer dummy")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	if err = resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("models status = %d, want 200", resp.StatusCode)
	}
}

func awaitExit(t *testing.T, result <-chan int) {
	t.Helper()
	select {
	case code := <-result:
		if code != 0 {
			t.Fatalf("exit = %d, want 0", code)
		}
	case <-time.After(6 * time.Second):
		t.Fatal("the command did not return after cancel")
	}
}
