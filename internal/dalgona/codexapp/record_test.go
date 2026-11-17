package codexapp

import (
	"bytes"
	"errors"
	"io"
	"io/fs"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
)

func buildEcho(t *testing.T) string {
	t.Helper()
	goTool, err := exec.LookPath("go")
	if err != nil {
		t.Skipf("go tool not on PATH: %v", err)
	}
	name := "echo"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	binary := filepath.Join(t.TempDir(), name)
	build := exec.CommandContext(t.Context(), goTool, "build", "-o", binary, "./testdata/echo")
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build echo: %v\n%s", err, output)
	}
	return binary
}

func writeInChunks(w *io.PipeWriter, data []byte, seed uint64) {
	random := rand.New(rand.NewPCG(seed, seed^0x9e3779b97f4a7c15))
	for len(data) > 0 {
		n := min(len(data), 1+random.IntN(9))
		if _, err := w.Write(data[:n]); err != nil {
			_ = w.CloseWithError(err)
			return
		}
		data = data[n:]
	}
	_ = w.Close()
}

func censusFile(t *testing.T, home string, before, after int64) string {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join(home, "census"))
	if err != nil || len(entries) != 1 {
		t.Fatalf("census entries = %v, %v", entries, err)
	}
	stamp, ok := strings.CutSuffix(entries[0].Name(), ".jsonl")
	unix, err := strconv.ParseInt(stamp, 10, 64)
	if !ok || err != nil || unix < before || unix > after {
		t.Fatalf("census file %q outside [%d, %d]", entries[0].Name(), before, after)
	}
	return filepath.Join(home, "census", entries[0].Name())
}

func censusLines(t *testing.T, home string, before, after int64) []string {
	t.Helper()
	data, err := os.ReadFile(censusFile(t, home, before, after))
	if err != nil {
		t.Fatalf("read census: %v", err)
	}
	var lines []string
	for line := range strings.Lines(string(data)) {
		trimmed, ok := strings.CutSuffix(line, "\n")
		if !ok {
			t.Fatalf("census line without newline: %q", line)
		}
		lines = append(lines, trimmed)
	}
	return lines
}

func censusByDirection(t *testing.T, lines []string) (toServer, toClient []string) {
	t.Helper()
	for _, line := range lines {
		switch {
		case strings.HasPrefix(line, `{"dir":"c2s"`):
			toServer = append(toServer, line)
		case strings.HasPrefix(line, `{"dir":"s2c"`):
			toClient = append(toClient, line)
		default:
			t.Fatalf("census line %q", line)
		}
	}
	return toServer, toClient
}

const echoReady = `{"method":"echo/ready"}` + "\n"

func expectedCensus(dir string) []string {
	return []string{
		`{"dir":"` + dir + `","method":"initialize","id":1}`,
		`{"dir":"` + dir + `","method":"initialized"}`,
		`{"dir":"` + dir + `","id":"req-2"}`,
		`{"dir":"` + dir + `","method":"thread/start","id":null}`,
		`{"dir":"` + dir + `","method":"turn/start","id":4}`,
		`{"dir":"` + dir + `","method":"exit"}`,
	}
}

func TestRecordPassesBytesThroughAndWritesCensus(t *testing.T) {
	echo := buildEcho(t)
	input := []byte(`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"t"}}}` + "\n" +
		`{"method":"initialized"}` + "\n" +
		"not json at all\n" +
		`{"id":"req-2","result":{"ok":true}}` + "\n" +
		"[1,2,3]\n" +
		`{"id":3,"method":"turn/start"} trailing` + "\n" +
		`{"jsonrpc":"2.0"}` + "\n" +
		"\xff\xfe raw bytes\n" +
		`{"\u006dethod":"thread/start","id":null}` + "\n" +
		`{"method":"turn/start","id":4}` + "\r\n" +
		`{"method":"exit"}`)

	for seed := range uint64(4) {
		home := t.TempDir()
		appIn, testIn := io.Pipe()
		go writeInChunks(testIn, input, seed)
		var stdout, stderr bytes.Buffer
		before := time.Now().Unix()
		code := RecordMain(t.Context(), []string{"--real", echo, "app-server", "--analytics-default-enabled"}, stdioPair{appIn, &stdout}, &stderr, home)
		after := time.Now().Unix()

		if code != 3 {
			t.Fatalf("seed %d: RecordMain = %d, stderr %q", seed, code, stderr.String())
		}
		if want := append([]byte(echoReady), input...); !bytes.Equal(stdout.Bytes(), want) {
			t.Fatalf("seed %d: stdout = %q, want %q", seed, stdout.Bytes(), want)
		}
		if !strings.Contains(stderr.String(), "args: app-server --analytics-default-enabled\n") {
			t.Fatalf("seed %d: stderr = %q", seed, stderr.String())
		}
		toServer, toClient := censusByDirection(t, censusLines(t, home, before, after))
		wantToClient := append([]string{`{"dir":"s2c","method":"echo/ready"}`}, expectedCensus("s2c")...)
		if !slices.Equal(toServer, expectedCensus("c2s")) || !slices.Equal(toClient, wantToClient) {
			t.Fatalf("seed %d: census c2s %q s2c %q", seed, toServer, toClient)
		}
	}
}

func TestRecordDefaultsToAppServerArgument(t *testing.T) {
	echo := buildEcho(t)
	home := t.TempDir()
	var stdout, stderr bytes.Buffer
	before := time.Now().Unix()
	code := RecordMain(t.Context(), []string{"--real=" + echo}, stdioPair{strings.NewReader(""), &stdout}, &stderr, home)
	after := time.Now().Unix()
	if code != 3 || stdout.String() != echoReady {
		t.Fatalf("RecordMain = %d, stdout %q, stderr %q", code, stdout.String(), stderr.String())
	}
	if !strings.Contains(stderr.String(), "args: app-server\n") {
		t.Fatalf("stderr = %q", stderr.String())
	}
	if lines := censusLines(t, home, before, after); !slices.Equal(lines, []string{`{"dir":"s2c","method":"echo/ready"}`}) {
		t.Fatalf("census = %q", lines)
	}
}

func TestRecordRequiresRealBinary(t *testing.T) {
	cases := []struct {
		args    []string
		message string
	}{
		{args: []string{"app-server"}, message: "dalgona codex record: --real <path-to-codex> is required\n"},
		{args: []string{"--real"}, message: "dalgona codex record: --real requires a value\n"},
	}
	for _, tc := range cases {
		home := t.TempDir()
		var stderr bytes.Buffer
		code := RecordMain(t.Context(), tc.args, stdioPair{strings.NewReader(""), io.Discard}, &stderr, home)
		if code != 2 || stderr.String() != tc.message {
			t.Errorf("%v: code %d stderr %q, want 2 %q", tc.args, code, stderr.String(), tc.message)
		}
		if _, err := os.Stat(filepath.Join(home, "census")); !errors.Is(err, fs.ErrNotExist) {
			t.Errorf("%v: census directory created: %v", tc.args, err)
		}
	}
}
