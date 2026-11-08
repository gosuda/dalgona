package webfetch

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestFetchHTML(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = fmt.Fprint(writer, `<html><head><style>hidden-style</style><script>hidden-script</script><noscript>hidden-noscript</noscript></head><body><h1>Hello &amp; welcome</h1><p>Visible <strong>text</strong>.</p><div>Next<br>line</div></body></html>`)
	}))
	t.Cleanup(server.Close)
	result := runLocal(t, server.URL, defaultMaxBytes)
	requireFetchOutput(t, result, "Hello & welcome Visible text. Next line")
}

func TestFetchPlainText(t *testing.T) {
	t.Parallel()
	const text = "first  line\nsecond\tline\n"
	server := textServer(t, "text/plain", text)
	result := runLocal(t, server.URL, defaultMaxBytes)
	requireFetchOutput(t, result, text)
}

func TestFetchBinary(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", "application/octet-stream")
		_, _ = writer.Write([]byte{0, 1, 2, 3})
	}))
	t.Cleanup(server.Close)
	result := runLocal(t, server.URL, defaultMaxBytes)
	requireFetchOutput(t, result, "[binary content application/octet-stream, 4 bytes]")
}

func TestFetchCapsOversizedBody(t *testing.T) {
	t.Parallel()
	server := textServer(t, "text/plain", "0123456789")
	result := runLocal(t, server.URL, 5)
	requireFetchOutput(t, result, "01234\n[content truncated at 5 bytes]")
}

func TestFetchFollowsRedirect(t *testing.T) {
	t.Parallel()
	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path == "/start" {
			http.Redirect(writer, request, server.URL+"/final", http.StatusFound)
			return
		}
		writer.Header().Set("Content-Type", "text/plain")
		_, _ = fmt.Fprint(writer, "arrived")
	}))
	t.Cleanup(server.Close)
	result := runLocal(t, server.URL+"/start", defaultMaxBytes)
	requireFetchOutput(t, result, "arrived")
}

func TestFetchReportsHTTPError(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.NotFoundHandler())
	t.Cleanup(server.Close)
	result := runLocal(t, server.URL, defaultMaxBytes)
	if result.OK || string(result.Output) != "web_fetch: HTTP 404 Not Found" {
		t.Fatalf("unexpected 404 result: %+v", result)
	}
}

func TestMainMatchesInProcessFetch(t *testing.T) {
	t.Parallel()
	server := textServer(t, "text/html", `<p>A &amp; B</p><script>hidden</script>`)
	direct := runLocal(t, server.URL, 1024)
	var output bytes.Buffer
	code := Main([]string{server.URL, "1024"}, &output)
	if code != 0 {
		t.Fatalf("Main exit code = %d, output = %q", code, output.String())
	}
	if !bytes.Equal(output.Bytes(), direct.Output) {
		t.Fatalf("Main output = %q, direct output = %q", output.Bytes(), direct.Output)
	}
}

func TestRemoteFetchUsesHiddenCommand(t *testing.T) {
	t.Parallel()
	run := &remoteRunner{output: runner.Output{Data: []byte("remote text"), Exit: &runner.Exit{Code: 0}, Path: "out"}}
	call := tool.Call{ID: "call-1", Args: []byte(`{"url":"https://example.test/doc","max_bytes":4096}`)}
	result, err := New().Run(t.Context(), call, tool.Env{Runner: run, Cwd: "/work"})
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || string(result.Output) != "remote text" {
		t.Fatalf("unexpected remote result: %+v", result)
	}
	want := []string{"@self", "__fetch", "https://example.test/doc", "4096"}
	if strings.Join(run.spec.Argv, "\x00") != strings.Join(want, "\x00") {
		t.Fatalf("argv = %q, want %q", run.spec.Argv, want)
	}
}

func TestFetchRejectsInvalidLimits(t *testing.T) {
	t.Parallel()
	for _, raw := range []string{`{"url":"https://example.test","max_bytes":0}`, `{"url":"https://example.test","max_bytes":1.5}`} {
		if _, err := parseArgs([]byte(raw)); err == nil {
			t.Fatalf("accepted arguments %s", raw)
		}
	}
}

func runLocal(t *testing.T, rawURL string, limit int64) tool.Result {
	t.Helper()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	raw := `{"url":` + strconv.Quote(rawURL) + `,"max_bytes":` + strconv.FormatInt(limit, 10) + `}`
	result, err := New().Run(t.Context(), tool.Call{Args: []byte(raw)}, tool.Env{Runner: local})
	if err != nil {
		t.Fatal(err)
	}
	return result
}

func requireFetchOutput(t *testing.T, result tool.Result, want string) {
	t.Helper()
	if !result.OK || string(result.Output) != want {
		t.Fatalf("fetch result = %+v, want output %q", result, want)
	}
}

func textServer(t *testing.T, contentType, body string) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", contentType)
		_, _ = fmt.Fprint(writer, body)
	}))
	t.Cleanup(server.Close)
	return server
}

type remoteRunner struct {
	runner.Runner
	spec   runner.ProcessSpec
	output runner.Output
}

func (r *remoteRunner) Start(_ context.Context, spec runner.ProcessSpec) error {
	r.spec = spec
	return nil
}

func (r *remoteRunner) Read(_ context.Context, _ string, _ int64) (runner.Output, error) {
	return r.output, nil
}
