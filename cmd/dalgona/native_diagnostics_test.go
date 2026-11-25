package main

import (
	"errors"
	"fmt"
	"io/fs"
	"maps"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/app"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
)

type nativeRig struct {
	home string
	peer *rpc.Conn
}

func newNativeRig(t *testing.T, before func(home string), options func(home string) rpcserver.ServeOptions) *nativeRig {
	t.Helper()
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	before(home)
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona})
	if err != nil {
		t.Fatalf("app host: %v", err)
	}
	serverConn, clientConn := net.Pipe()
	done := make(chan struct{})
	go func() {
		defer close(done)
		_ = rpcserver.ServeWith(t.Context(), serverConn, host, options(home))
		_ = closeHost()
	}()
	peer := rpc.Dial(t.Context(), clientConn, quietClient{})
	t.Cleanup(func() {
		_ = peer.Close()
		_ = clientConn.Close()
		<-done
	})
	return &nativeRig{home: home, peer: peer}
}

func (r *nativeRig) refused(t *testing.T, method string, params any, kind rpc.ErrorKind) rpc.ErrorData {
	t.Helper()
	err := r.peer.Call(t.Context(), method, params, nil)
	data, native := rpc.ErrorDataOf(err)
	coded, isRPC := errors.AsType[*rpc.RPCError](err)
	if !native || !isRPC || data.Kind != kind || coded.Message != string(kind) {
		t.Fatalf("%s %v: got %v, want %s", method, params, err, kind)
	}
	return data
}

type fileState struct {
	size    int64
	mode    fs.FileMode
	modTime time.Time
}

func treeState(t *testing.T, root string) map[string]fileState {
	t.Helper()
	state := map[string]fileState{}
	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil || isHomeLockFile(entry.Name()) {
			return walkErr
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		state[path] = fileState{size: info.Size(), mode: info.Mode(), modTime: info.ModTime()}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return state
}

type diagnosticRow struct {
	Status string `json:"status"`
	Check  string `json:"check"`
	Detail string `json:"detail"`
}

type diagnosticsResult struct {
	Rows     []diagnosticRow `json:"rows"`
	ExitCode int             `json:"exitCode"`
}

func (d diagnosticsResult) checks() []string {
	var checks []string
	for _, row := range d.Rows {
		if len(checks) == 0 || checks[len(checks)-1] != row.Check {
			checks = append(checks, row.Check)
		}
	}
	return checks
}

func clearProviderEnvironment(t *testing.T) {
	t.Helper()
	for _, name := range []string{
		"OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY", "DEVIN_API_KEY",
		"CHATGPT_API_KEY", "ANTIGRAVITY_API_KEY", "CURSOR_API_KEY", "OPENAI_BASE_URL",
	} {
		t.Setenv(name, "")
	}
}

func TestDiagnosticsRunReturnsDoctorRowsWithoutWriting(t *testing.T) {
	clearProviderEnvironment(t)
	var probes atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		probes.Add(1)
		fmt.Fprint(w, `{"data":[{"id":"m1"},{"id":"m2"}]}`)
	}))
	t.Cleanup(server.Close)
	t.Setenv("STUB_KEY", "sk-stub-diagnostics")
	rig := newNativeRig(t, seedBrokenPluginHome(t, server), func(home string) rpcserver.ServeOptions {
		return rpcserver.ServeOptions{Extensions: diagnosticsExtensions(home)}
	})
	before := treeState(t, rig.home)
	wantChecks := []string{"home", "config", "daemon", "build", "threads", "providers", "sandbox", "tls", "mcp", "plugins"}
	requireOfflineDiagnostics(t, rig, &probes, wantChecks)
	requireOnlineDiagnostics(t, rig, &probes, wantChecks, before)
	requireDiagnosticsReject(t, rig)
}

func seedBrokenPluginHome(t *testing.T, server *httptest.Server) func(string) {
	t.Helper()
	return func(home string) {
		text := fmt.Sprintf("[providers.stub]\nbase_url = %q\napi_key_env = \"STUB_KEY\"\n", server.URL)
		if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := os.Mkdir(filepath.Join(home, "plugins"), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(home, "plugins", "broken.star"), []byte("def (\n"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

func requireOfflineDiagnostics(t *testing.T, rig *nativeRig, probes *atomic.Int32, wantChecks []string) {
	t.Helper()
	var offline diagnosticsResult
	if err := rig.peer.Call(t.Context(), methodDiagnosticsRun, map[string]any{"online": false}, &offline); err != nil {
		t.Fatal(err)
	}
	if got := offline.checks(); !slices.Equal(got, wantChecks) || probes.Load() != 0 {
		t.Fatalf("offline checks = %v with %d probes, want %v and none", got, probes.Load(), wantChecks)
	}
	last := offline.Rows[len(offline.Rows)-1]
	if last.Check != "plugins" || last.Status != "fail" || !strings.Contains(last.Detail, "broken.star") || offline.ExitCode != 1 {
		t.Fatalf("broken plugin gave row %+v and exit code %d, want a failing plugins row and exit code 1", last, offline.ExitCode)
	}
}

func requireOnlineDiagnostics(t *testing.T, rig *nativeRig, probes *atomic.Int32, wantChecks []string, before map[string]fileState) {
	t.Helper()
	var online diagnosticsResult
	if err := rig.peer.Call(t.Context(), methodDiagnosticsRun, map[string]any{"online": true}, &online); err != nil {
		t.Fatal(err)
	}
	wantOnline := slices.Insert(slices.Clone(wantChecks), 6, "provider-models")
	if got := online.checks(); !slices.Equal(got, wantOnline) || probes.Load() != 1 {
		t.Fatalf("online checks = %v with %d probes, want %v and one", got, probes.Load(), wantOnline)
	}
	after := treeState(t, rig.home)
	if !maps.EqualFunc(before, after, func(a, b fileState) bool { return a == b }) {
		t.Fatalf("diagnostics changed the home: %s", homeChanges(before, after))
	}
}

func homeChanges(before, after map[string]fileState) string {
	var changes []string
	for path, state := range after {
		previous, existed := before[path]
		switch {
		case !existed:
			changes = append(changes, path+" created")
		case previous != state:
			changes = append(changes, path+" changed")
		}
	}
	for path := range before {
		if _, exists := after[path]; !exists {
			changes = append(changes, path+" removed")
		}
	}
	slices.Sort(changes)
	return strings.Join(changes, ", ")
}

func requireDiagnosticsReject(t *testing.T, rig *nativeRig) {
	t.Helper()
	for _, params := range []any{map[string]any{}, map[string]any{"online": "yes"}} {
		rig.refused(t, methodDiagnosticsRun, params, rpc.ErrorInvalidParams)
	}
}
