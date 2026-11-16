package doctor

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json/jsontext"
	"encoding/pem"
	"errors"
	"fmt"
	"io/fs"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/daemon"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const own = "0123456789abcdef"

const stubKey = "sk-stub-secret-0001"

func fixedNow() time.Time { return time.Date(2026, 11, 15, 12, 0, 0, 0, time.UTC) }

func credentialEnv() []string {
	return []string{
		"OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY", "DEVIN_API_KEY",
		"CHATGPT_API_KEY", "ANTIGRAVITY_API_KEY", "CURSOR_API_KEY", "OPENAI_BASE_URL",
	}
}

func newHome(t *testing.T) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("the home mode check runs on unix only")
	}
	home := filepath.Join(t.TempDir(), "home")
	if err := os.Mkdir(home, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_HOME", home)
	for _, name := range credentialEnv() {
		t.Setenv(name, "")
	}
	return home
}

func writeFile(t *testing.T, path, text string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(text), 0o600); err != nil {
		t.Fatal(err)
	}
}

func storeOf(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func noDaemon(context.Context) (net.Conn, error) { return nil, fs.ErrNotExist }

type harness struct {
	in  *inspector
	out *bytes.Buffer
}

func newHarness(t *testing.T, home string) *harness {
	t.Helper()
	out := &bytes.Buffer{}
	in := newInspector(Env{Home: home, Dial: noDaemon, Own: own, Now: fixedNow, Out: out})
	in.sandboxCause = func() string { return "" }
	in.goos = "linux"
	return &harness{in: in, out: out}
}

func (h *harness) run(t *testing.T) ([]string, int) {
	t.Helper()
	code := h.in.run(t.Context())
	return strings.Split(strings.TrimRight(h.out.String(), "\n"), "\n"), code
}

func only(lines []string, checks ...string) []string {
	var kept []string
	for _, line := range lines {
		_, rest, _ := strings.Cut(line, " ")
		check, _, _ := strings.Cut(rest, ":")
		if slices.Contains(checks, check) {
			kept = append(kept, line)
		}
	}
	return kept
}

func expectLines(t *testing.T, got, want []string) {
	t.Helper()
	if !slices.Equal(got, want) {
		t.Fatalf("rows:\n%s\nwant:\n%s", strings.Join(got, "\n"), strings.Join(want, "\n"))
	}
}

func expectCode(t *testing.T, got, want int) {
	t.Helper()
	if got != want {
		t.Fatalf("exit code = %d, want %d", got, want)
	}
}

func TestRowsOfAHealthyHome(t *testing.T) {
	home := newHome(t)
	writeFile(t, filepath.Join(home, "config.toml"), "[providers.stub]\nbase_url = \"http://127.0.0.1:1\"\napi_key_env = \"STUB_API_KEY\"\n")
	t.Setenv("STUB_API_KEY", "")
	if err := storeOf(t, home).SetAPIKey("stub", stubKey); err != nil {
		t.Fatal(err)
	}
	got, code := newHarness(t, home).run(t)
	expectLines(t, got, []string{
		"ok home: " + home,
		"ok config: valid",
		"ok daemon: not running; it starts on demand",
		"ok build: " + own,
		"ok threads: 0 threads",
		"ok providers: stub: api key set",
		"ok sandbox: Landlock",
		"ok tls: system roots",
		"ok mcp: none configured",
		"ok plugins: 0 loaded",
	})
	expectCode(t, code, 0)
}

func TestLooseHomeFailsTheRun(t *testing.T) {
	home := newHome(t)
	if err := os.Chmod(home, 0o500); err != nil { //nolint:gosec // 0o500 is a directory permission; the test needs a wrong home mode
		t.Fatal(err)
	}
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "home"), []string{"fail home: daemon home mode 0500 is not 0700"})
	expectCode(t, code, 1)
}

func TestConfigErrorFailsAndSkipsDependentRows(t *testing.T) {
	home := newHome(t)
	writeFile(t, filepath.Join(home, "config.toml"), "bogus = 1\n")
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "config", "tls", "mcp"), []string{
		"fail config: config.toml:1: unknown key \"bogus\"",
		"warn tls: not checked; the config did not load",
		"warn mcp: not checked; the config did not load",
	})
	expectCode(t, code, 1)
}

func pipeTo(t *testing.T, handler rpc.Handler) func(context.Context) (net.Conn, error) {
	t.Helper()
	return func(context.Context) (net.Conn, error) {
		client, server := net.Pipe()
		conn := rpc.NewConn(t.Context(), server, handler)
		t.Cleanup(func() { _ = conn.Close() })
		return client, nil
	}
}

func identityDaemon(t *testing.T, build string) func(context.Context) (net.Conn, error) {
	t.Helper()
	reply := fmt.Sprintf(`{"build":%q,"pid":4242,"version":"test","startedAt":"2026-11-15T11:58:55Z"}`, build)
	return pipeTo(t, func(_ context.Context, method string, _ jsontext.Value) (jsontext.Value, error) {
		if method != daemon.MethodIdentity {
			return nil, &rpc.RPCError{Code: -32601, Message: "method not found"}
		}
		return jsontext.Value(reply), nil
	})
}

func TestRunningDaemonWithEqualBuild(t *testing.T) {
	home := newHome(t)
	h := newHarness(t, home)
	h.in.env.Dial = identityDaemon(t, own)
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build"), []string{"ok daemon: running pid 4242 up 1m 05s", "ok build: " + own})
	expectCode(t, code, 0)
}

func TestRunningDaemonWithOtherBuildFails(t *testing.T) {
	home := newHome(t)
	h := newHarness(t, home)
	h.in.env.Dial = identityDaemon(t, "aaaaaaaaaaaaaaaa")
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build"), []string{
		"ok daemon: running pid 4242 up 1m 05s",
		"fail build: daemon runs build aaaaaaaaaaaaaaaa, this binary is build " + own + "; run dalgona restart",
	})
	expectCode(t, code, 1)
}

func TestPreIdentityDaemonFailsTheBuildRow(t *testing.T) {
	home := newHome(t)
	h := newHarness(t, home)
	h.in.env.Dial = pipeTo(t, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return nil, &rpc.RPCError{Code: -32601, Message: "method not found"}
	})
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build"), []string{
		"ok daemon: running; it reports no identity",
		"fail build: daemon runs build " + daemon.UnknownPreIdentityBuild + ", this binary is build " + own + "; run dalgona restart",
	})
	expectCode(t, code, 1)
}

func TestDaemonThatCannotBeInspectedFails(t *testing.T) {
	home := newHome(t)
	h := newHarness(t, home)
	h.in.env.Dial = pipeTo(t, func(context.Context, string, jsontext.Value) (jsontext.Value, error) {
		return nil, errors.New("boom")
	})
	got, code := h.run(t)
	rows := only(got, "daemon", "build")
	if len(rows) != 2 || !strings.HasPrefix(rows[0], "fail daemon: cannot inspect the running daemon: ") {
		t.Fatalf("rows = %q", rows)
	}
	if rows[1] != "warn build: not checked; the daemon cannot be inspected" {
		t.Fatalf("build row = %q", rows[1])
	}
	expectCode(t, code, 1)
}

func TestStaleSocketWarns(t *testing.T) {
	home := newHome(t)
	writeFile(t, filepath.Join(home, "daemon.log"), "panic: boom\n")
	writeFile(t, filepath.Join(home, "dalgona.sock"), "")
	h := newHarness(t, home)
	h.in.env.Dial = func(context.Context) (net.Conn, error) { return nil, errors.New("connection refused") }
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build", "daemon-log"), []string{
		"warn daemon: stale socket " + filepath.Join(home, "dalgona.sock") + "; any dalgona command replaces it",
		"ok build: " + own,
		"warn daemon-log: panic: boom",
	})
	expectCode(t, code, 0)
}

func TestUnreachableSocketPathFailsWithoutALogDump(t *testing.T) {
	home := newHome(t)
	writeFile(t, filepath.Join(home, "daemon.log"), "panic: boom\n")
	h := newHarness(t, home)
	h.in.env.Dial = func(context.Context) (net.Conn, error) { return nil, errors.New("invalid argument") }
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build", "daemon-log"), []string{
		"fail daemon: cannot reach the socket: invalid argument",
		"warn build: not checked; the socket cannot be reached",
	})
	expectCode(t, code, 1)
}

type untrustedError struct{}

func (untrustedError) Error() string { return "daemon: untrusted socket: uid 1 is not 2" }

func (untrustedError) Is(target error) bool { return daemon.Untrusted(target) }

func TestUntrustedSocketFailsAndShowsTheLastTwentyLogLines(t *testing.T) {
	home := newHome(t)
	if err := storeOf(t, home).SetAPIKey("stub", stubKey); err != nil {
		t.Fatal(err)
	}
	var log strings.Builder
	for line := 1; line <= 24; line++ {
		fmt.Fprintf(&log, "event %02d\n", line)
	}
	log.WriteString("refused " + stubKey + "\n")
	writeFile(t, filepath.Join(home, "daemon.log"), log.String())
	h := newHarness(t, home)
	h.in.env.Dial = func(context.Context) (net.Conn, error) { return nil, untrustedError{} }
	got, code := h.run(t)
	want := []string{
		"fail daemon: the socket belongs to another user: daemon: untrusted socket: uid 1 is not 2",
		"warn build: not checked; the socket is not trusted",
	}
	for line := 6; line <= 24; line++ {
		want = append(want, fmt.Sprintf("warn daemon-log: event %02d", line))
	}
	want = append(want, "warn daemon-log: refused {{secret:auth:stub}}")
	expectLines(t, only(got, "daemon", "build", "daemon-log"), want)
	expectCode(t, code, 1)
}

func TestUnreadableDaemonLogWarnsBesideTheFailure(t *testing.T) {
	home := newHome(t)
	h := newHarness(t, home)
	h.in.env.Dial = func(context.Context) (net.Conn, error) { return nil, untrustedError{} }
	got, code := h.run(t)
	expectLines(t, only(got, "daemon", "build", "daemon-log"), []string{
		"fail daemon: the socket belongs to another user: daemon: untrusted socket: uid 1 is not 2",
		"warn build: not checked; the socket is not trusted",
		"warn daemon-log: read daemon.log: open " + filepath.Join(home, "daemon.log") + ": no such file or directory",
	})
	expectCode(t, code, 1)
}

func TestLogRowsAreOnePlainBoundedLine(t *testing.T) {
	home := newHome(t)
	writeFile(t, filepath.Join(home, "daemon.log"), "\x1b[31mred\x1b[0m\n"+strings.Repeat("x", 1000)+"\n")
	h := newHarness(t, home)
	h.in.env.Dial = func(context.Context) (net.Conn, error) { return nil, untrustedError{} }
	got, _ := h.run(t)
	if strings.Contains(h.out.String(), "\x1b") {
		t.Fatalf("output holds an escape byte: %q", h.out.String())
	}
	expectLines(t, only(got, "daemon-log"), []string{
		"warn daemon-log: [31mred [0m",
		"warn daemon-log: " + strings.Repeat("x", maxDetail) + "...",
	})
}

func TestThreadRows(t *testing.T) {
	home := newHome(t)
	for _, id := range []string{"a", "b"} {
		writeFile(t, filepath.Join(home, "sessions", id, "journal.jsonl"), "")
	}
	writeFile(t, filepath.Join(home, "sessions", "c", "notes.txt"), "x")
	writeFile(t, filepath.Join(home, "sessions", "loose.txt"), "x")
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "threads"), []string{"ok threads: 2 threads"})
	expectCode(t, code, 0)

	writeFile(t, filepath.Join(home, "sessions", "b", "damaged"), "7\n")
	got, code = newHarness(t, home).run(t)
	expectLines(t, only(got, "threads"), []string{"warn threads: 2 threads, 1 damaged: b"})
	expectCode(t, code, 0)

	for index := 1; index <= 7; index++ {
		id := fmt.Sprintf("d%d", index)
		writeFile(t, filepath.Join(home, "sessions", id, "journal.jsonl"), "")
		writeFile(t, filepath.Join(home, "sessions", id, "damaged"), "1\n")
	}
	got, _ = newHarness(t, home).run(t)
	expectLines(t, only(got, "threads"), []string{"warn threads: 9 threads, 8 damaged: b, d1, d2, d3, d4"})
}

func TestProviderRows(t *testing.T) {
	home := newHome(t)
	t.Setenv("ANTHROPIC_API_KEY", "sk-env-secret-0002")
	store := storeOf(t, home)
	for id, credential := range map[string]auth.Credential{
		"chatgpt": {Type: "oauth", Access: "chatgpt-access-token", Expires: fixedNow().Add(90 * time.Minute).Format(time.RFC3339)},
		"cursor":  {Type: "oauth", Access: "cursor-access-token", Refresh: "cursor-refresh-token", Expires: fixedNow().Add(-time.Minute).Format(time.RFC3339)},
		"devin":   {Type: "oauth", Access: "devin-access-token", Expires: fixedNow().Add(-time.Minute).Format(time.RFC3339)},
		"openai":  {Type: "oauth", Access: "openai-access-token", Expires: "tomorrow"},
	} {
		if err := store.SetCredential(id, credential); err != nil {
			t.Fatal(err)
		}
	}
	h := newHarness(t, home)
	got, code := h.run(t)
	expectLines(t, only(got, "providers"), []string{
		"ok providers: anthropic: api key set",
		"ok providers: chatgpt: expires in 1h 30m",
		"warn providers: cursor: expired, refreshes on next use",
		"fail providers: devin: expired; run dalgona login devin",
		"fail providers: openai: expiry \"tomorrow\" is not valid; run dalgona login openai",
	})
	expectCode(t, code, 1)
	for _, secret := range []string{"chatgpt-access-token", "cursor-refresh-token", "sk-env-secret-0002"} {
		if strings.Contains(h.out.String(), secret) {
			t.Fatalf("output holds the credential %q", secret)
		}
	}
}

func TestNoCredentialWarnsOnce(t *testing.T) {
	home := newHome(t)
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "providers"), []string{"warn providers: no credential found; run dalgona login <id>"})
	expectCode(t, code, 0)
}

func TestSandboxRows(t *testing.T) {
	home := newHome(t)
	unavailable := func() string { return "Landlock is not available on this kernel" }

	h := newHarness(t, home)
	h.in.sandboxCause = unavailable
	got, code := h.run(t)
	expectLines(t, only(got, "sandbox"), []string{"warn sandbox: Landlock is not available on this kernel; commands ask for a sandbox choice"})
	expectCode(t, code, 0)

	h = newHarness(t, home)
	h.in.goos = "darwin"
	got, _ = h.run(t)
	expectLines(t, only(got, "sandbox"), []string{"ok sandbox: sandbox-exec"})

	writeFile(t, filepath.Join(home, "config.toml"), "sandbox = \"off\"\n")
	h = newHarness(t, home)
	h.in.sandboxCause = unavailable
	got, _ = h.run(t)
	expectLines(t, only(got, "sandbox"), []string{"ok sandbox: off by configuration"})
}

func writeCertificates(t *testing.T, path string, count int) {
	t.Helper()
	var data []byte
	for serial := range count {
		key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
		if err != nil {
			t.Fatal(err)
		}
		template := &x509.Certificate{
			SerialNumber:          big.NewInt(int64(serial + 1)),
			Subject:               pkix.Name{CommonName: "doctor test"},
			NotBefore:             fixedNow().Add(-time.Hour),
			NotAfter:              fixedNow().Add(time.Hour),
			IsCA:                  true,
			BasicConstraintsValid: true,
			KeyUsage:              x509.KeyUsageCertSign,
		}
		der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
		if err != nil {
			t.Fatal(err)
		}
		data = append(data, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})...)
	}
	writeFile(t, path, string(data))
}

func TestTLSRows(t *testing.T) {
	home := newHome(t)
	ca := filepath.Join(home, "ca.pem")
	writeCertificates(t, ca, 2)
	writeFile(t, filepath.Join(home, "config.toml"), fmt.Sprintf("tls_ca_file = %q\n", ca))
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "tls"), []string{"ok tls: 2 certificates from " + ca})
	expectCode(t, code, 0)

	writeFile(t, ca, "not a certificate")
	got, code = newHarness(t, home).run(t)
	rows := only(got, "config", "tls")
	if len(rows) != 2 || !strings.HasPrefix(rows[0], "fail config: ") || !strings.Contains(rows[0], "tls_ca_file") {
		t.Fatalf("rows = %q", rows)
	}
	if rows[1] != "warn tls: not checked; the config did not load" {
		t.Fatalf("tls row = %q", rows[1])
	}
	expectCode(t, code, 1)
}

func TestMCPRows(t *testing.T) {
	home := newHome(t)
	got, _ := newHarness(t, home).run(t)
	expectLines(t, only(got, "mcp"), []string{"ok mcp: none configured"})

	writeFile(t, filepath.Join(home, "config.toml"), "[mcp_servers.beta]\nurl = \"https://example.invalid/mcp\"\n\n[mcp_servers.alpha]\ncommand = \"echo\"\n")
	got, code := newHarness(t, home).run(t)
	expectLines(t, only(got, "mcp"), []string{"ok mcp: alpha: stdio", "ok mcp: beta: http"})
	expectCode(t, code, 0)
}

func TestPluginRows(t *testing.T) {
	const good = "def impl(ctx):\n    return 1\ncommand('hi', 'Say hi.', impl)\n"
	t.Run("loaded", func(t *testing.T) {
		home := newHome(t)
		writeFile(t, filepath.Join(home, "plugins", "good.star"), good)
		got, code := newHarness(t, home).run(t)
		expectLines(t, only(got, "plugins"), []string{"ok plugins: 1 loaded"})
		expectCode(t, code, 0)
	})
	t.Run("top-level effect", func(t *testing.T) {
		home := newHome(t)
		writeFile(t, filepath.Join(home, "plugins", "good.star"), good)
		writeFile(t, filepath.Join(home, "plugins", "bad.star"), "started = time.time()\n")
		got, code := newHarness(t, home).run(t)
		expectLines(t, only(got, "plugins"), []string{"fail plugins: plugin bad.star: effects are not available during inspection"})
		expectCode(t, code, 1)
	})
	t.Run("syntax error", func(t *testing.T) {
		home := newHome(t)
		writeFile(t, filepath.Join(home, "plugins", "broken.star"), "x = (\n")
		got, code := newHarness(t, home).run(t)
		rows := only(got, "plugins")
		if len(rows) != 1 || !strings.HasPrefix(rows[0], "fail plugins: plugin broken.star: ") {
			t.Fatalf("rows = %q", rows)
		}
		expectCode(t, code, 1)
	})
}

func TestFormatDuration(t *testing.T) {
	for _, tc := range []struct {
		d    time.Duration
		want string
	}{
		{0, "0 ms"},
		{999 * time.Millisecond, "999 ms"},
		{time.Second, "1s"},
		{59 * time.Second, "59s"},
		{time.Minute, "1m 00s"},
		{65 * time.Second, "1m 05s"},
		{59*time.Minute + 59*time.Second, "59m 59s"},
		{time.Hour, "1h 00m"},
		{90 * time.Minute, "1h 30m"},
		{100 * time.Hour, "100h 00m"},
	} {
		if got := formatDuration(tc.d); got != tc.want {
			t.Errorf("formatDuration(%v) = %q, want %q", tc.d, got, tc.want)
		}
	}
}
