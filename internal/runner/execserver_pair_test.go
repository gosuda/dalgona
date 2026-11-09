//go:build !windows

package runner

import (
	"bufio"
	"encoding/base64"
	"encoding/json/v2"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"testing"

	"github.com/coder/websocket"
)

const pairContents = "baseline"

func TestExecWS_servesPlainJSONClient(t *testing.T) {
	root := t.TempDir()
	local, err := NewLocal(filepath.Join(root, "runs"))
	if err != nil {
		t.Fatalf("new local: %v", err)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	token := "pair-token"
	ctx := t.Context()
	done := make(chan error, 1)
	go func() { done <- serveExecWS(ctx, listener, local, token, nil) }()
	t.Cleanup(func() { stopContractServer(t, listener, done) })

	ws, _, err := websocket.Dial(ctx, "ws://"+listener.Addr().String()+"/", &websocket.DialOptions{
		HTTPHeader: http.Header{"Authorization": []string{"Bearer " + token}},
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	conn := websocket.NetConn(ctx, ws, websocket.MessageText)
	defer func() { _ = conn.Close() }()

	encoded := base64.StdEncoding.EncodeToString([]byte(pairContents))
	line := sendPairRead(t, conn, writePairFixture(t, root))
	assertPairReply(t, line, encoded)
}

func writePairFixture(t *testing.T, root string) string {
	t.Helper()
	path := filepath.Join(root, "probe.txt")
	if err := os.WriteFile(path, []byte(pairContents), 0o600); err != nil {
		t.Fatalf("write fixture: %v", err)
	}
	return path
}

func sendPairRead(t *testing.T, conn io.ReadWriteCloser, path string) []byte {
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

func assertPairReply(t *testing.T, line []byte, want string) {
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
