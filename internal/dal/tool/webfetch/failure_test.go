package webfetch

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const remoteFetchArgs = `{"url":"https://example.test/doc","max_bytes":4096}`

type refusingStartRunner struct {
	remoteRunner
	err error
}

func (r *refusingStartRunner) Start(context.Context, runner.ProcessSpec) error { return r.err }

type contextReadRunner struct{ remoteRunner }

func (r *contextReadRunner) Read(ctx context.Context, _ string, _ int64) (runner.Output, error) {
	if err := ctx.Err(); err != nil {
		return runner.Output{}, err
	}
	return runner.Output{Exit: &runner.Exit{Code: -1}}, nil
}

func TestRemoteHelperFailureIsInternal(t *testing.T) {
	exited := runner.Output{Exit: &runner.Exit{Code: -1}}
	tests := []struct {
		name string
		run  runner.Runner
		id   string
	}{
		{name: "start", run: &refusingStartRunner{err: errors.New("remote: connection closed")}, id: "fetch-start"},
		{name: "read", run: &failReadRemoteRunner{Runner: &remoteRunner{output: exited}, remaining: 1}, id: "fetch-read"},
		{name: "missing call id", run: &remoteRunner{output: exited}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			call := tool.Call{ID: test.id, Args: []byte(remoteFetchArgs)}
			result, err := New().Run(t.Context(), call, tool.Env{Runner: test.run, Cwd: t.TempDir()})
			if err == nil {
				t.Fatal("the helper failure returned no error")
			}
			if result.Failure != tool.FailureInternal {
				t.Fatalf("failure = %q, want a helper failure marked internal (error: %v)", result.Failure, err)
			}
		})
	}
}

func TestRemoteFetchOrdinaryFailuresStayOrdinary(t *testing.T) {
	t.Run("site error", func(t *testing.T) {
		run := &remoteRunner{output: runner.Output{Data: []byte("web_fetch: HTTP 404 Not Found"), Exit: &runner.Exit{Code: 1}}}
		result, err := New().Run(t.Context(), tool.Call{ID: "fetch-404", Args: []byte(remoteFetchArgs)}, tool.Env{Runner: run, Cwd: t.TempDir()})
		if err != nil || result.OK {
			t.Fatalf("result = %+v, %v; want a failed fetch result", result, err)
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q, want a site error to stay ordinary", result.Failure)
		}
	})
	t.Run("cancelled", func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		cancel()
		result, err := New().Run(ctx, tool.Call{ID: "fetch-cancelled", Args: []byte(remoteFetchArgs)}, tool.Env{Runner: &contextReadRunner{}, Cwd: t.TempDir()})
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("error = %v, want cancellation", err)
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q, want a cancelled fetch to stay ordinary", result.Failure)
		}
	})
	t.Run("invalid arguments", func(t *testing.T) {
		result, err := New().Run(t.Context(), tool.Call{ID: "fetch-args", Args: []byte(`{"url":"ftp://example.test"}`)}, tool.Env{Runner: &remoteRunner{}, Cwd: t.TempDir()})
		if err == nil {
			t.Fatal("an invalid URL was accepted")
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q, want invalid arguments to stay ordinary", result.Failure)
		}
	})
}

func TestLocalFetchSiteErrorStaysOrdinary(t *testing.T) {
	server := httptest.NewServer(http.NotFoundHandler())
	t.Cleanup(server.Close)
	result := runLocal(t, server.URL, defaultMaxBytes)
	if result.OK {
		t.Fatalf("result = %+v, want a failed fetch", result)
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want a site error to stay ordinary", result.Failure)
	}
}
