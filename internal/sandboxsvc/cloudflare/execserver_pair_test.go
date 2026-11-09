package cloudflare

import (
	"bufio"
	"encoding/base64"
	"encoding/json/v2"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

const pairContents = "baseline"

func TestServiceConnectPairsWithExecServer(t *testing.T) {
	t.Parallel()
	local, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatalf("new local: %v", err)
	}
	service := New(newExecServer(t, local).URL, "token")
	conn := connectExecServer(t, service)
	defer func() { _ = conn.Close() }()
	encoded := base64.StdEncoding.EncodeToString([]byte(pairContents))
	assertReadReply(t, sendReadRequest(t, conn, writePairingFixture(t)), encoded)
}

func newExecServer(t *testing.T, local *runner.Local) *httptest.Server {
	t.Helper()
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		r.URL.Path = "/"
		runner.ExecServerHandler(t.Context(), local, "token", nil).ServeHTTP(w, r)
	})
	server := httptest.NewServer(handler)
	t.Cleanup(server.Close)
	return server
}

func writePairingFixture(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "probe.txt")
	if err := os.WriteFile(path, []byte(pairContents), 0o600); err != nil {
		t.Fatalf("write fixture: %v", err)
	}
	return path
}

func connectExecServer(t *testing.T, service *Service) io.ReadWriteCloser {
	t.Helper()
	conn, err := service.Connect(t.Context(), "box-1", sandboxsvc.ExecServer)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	return conn
}

func sendReadRequest(t *testing.T, conn io.ReadWriteCloser, path string) []byte {
	t.Helper()
	request := fmt.Sprintf(`{"id":1,"method":"fs/readFile","params":{"path":%q}}`+"\n", path)
	if _, err := conn.Write([]byte(request)); err != nil {
		t.Fatalf("write request: %v", err)
	}
	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		t.Fatalf("read response: %v", err)
	}
	return line
}

func assertReadReply(t *testing.T, line []byte, want string) {
	t.Helper()
	var response struct {
		ID     int `json:"id"`
		Result struct {
			DataBase64 string `json:"dataBase64"`
		} `json:"result"`
		Error *struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(line, &response); err != nil {
		t.Fatalf("decode response %q: %v", line, err)
	}
	if response.Error != nil {
		t.Fatalf("response error: code %d message %q", response.Error.Code, response.Error.Message)
	}
	if response.ID != 1 || response.Result.DataBase64 != want {
		t.Fatalf("response = (id %d, data %q), want (id 1, data %q)", response.ID, response.Result.DataBase64, want)
	}
}
