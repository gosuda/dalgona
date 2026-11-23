package rpcserver

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestFsWriteFileMissingParentIsNoSuchFile(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	path := filepath.Join(t.TempDir(), "missing", "file.txt")
	c.fails("fs/writeFile", map[string]any{"path": path, "dataBase64": "eA=="}, -32602, "no such file: "+path)
}

func TestFsRefusalsGiveTheProtocolText(t *testing.T) {
	dir := t.TempDir()
	plain := filepath.Join(dir, "plain.txt")
	if err := os.WriteFile(plain, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	over := filepath.Join(dir, "over.bin")
	truncateTo(t, over, 16<<20+1)
	target := filepath.Join(dir, "target.txt")
	cases := []struct {
		name   string
		method string
		params map[string]any
		want   string
	}{
		{"invalid base64 gives dataBase64 is not valid base64", "fs/writeFile", map[string]any{"path": target, "dataBase64": "!!!!"}, "dataBase64 is not valid base64"},
		{"a file over 16 MiB gives file larger than 16 MiB", "fs/readFile", map[string]any{"path": over}, "file larger than 16 MiB"},
		{"a path that is not a directory gives not a directory: <path>", "fs/readDirectory", map[string]any{"path": plain}, "not a directory: " + plain},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newRig(t, config.ApprovalAuto).connect()
			c.fails(tc.method, tc.params, -32602, tc.want)
		})
	}
	if _, err := os.Lstat(target); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("refused fs/writeFile left %s behind: %v", target, err)
	}
}

func TestFsFileLimitIs16MiBForReadAndWrite(t *testing.T) {
	cases := []struct {
		name  string
		check func(t *testing.T, path string, size int64)
		size  int64
	}{
		{"fs/readFile accepts a file of 16 MiB", checkReadAccepted, 16 << 20},
		{"fs/writeFile accepts a file of 16 MiB", checkWriteAccepted, 16 << 20},
		{"fs/writeFile refuses a file over 16 MiB with file larger than 16 MiB", checkWriteRefused, 16<<20 + 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			tc.check(t, filepath.Join(t.TempDir(), "file.bin"), tc.size)
		})
	}
}

func checkReadAccepted(t *testing.T, path string, size int64) {
	t.Helper()
	truncateTo(t, path, size)
	result, err := (&server{}).fsReadFile(context.Background(), fsParamsOf(t, path, ""))
	if err != nil {
		t.Fatalf("fs/readFile of %d bytes: %v", size, err)
	}
	if want := base64.StdEncoding.EncodedLen(int(size)); len(result) < want {
		t.Fatalf("fs/readFile of %d bytes returned %d bytes, want at least %d", size, len(result), want)
	}
}

func checkWriteAccepted(t *testing.T, path string, size int64) {
	t.Helper()
	if _, err := (&server{}).fsWriteFile(context.Background(), fsParamsOf(t, path, base64OfZ(int(size)))); err != nil {
		t.Fatalf("fs/writeFile of %d bytes: %v", size, err)
	}
	if info, err := os.Stat(path); err != nil || info.Size() != size {
		t.Fatalf("fs/writeFile of %d bytes left %v, %v", size, info, err)
	}
}

func checkWriteRefused(t *testing.T, path string, size int64) {
	t.Helper()
	_, err := (&server{}).fsWriteFile(context.Background(), fsParamsOf(t, path, base64OfZ(int(size))))
	var coded *rpc.RPCError
	if !errors.As(err, &coded) || coded.Code != -32602 || coded.Message != "file larger than 16 MiB" {
		t.Fatalf("fs/writeFile of %d bytes: got %v, want -32602 %q", size, err, "file larger than 16 MiB")
	}
	if _, err := os.Lstat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("refused fs/writeFile left %s behind: %v", path, err)
	}
}

func base64OfZ(size int) string {
	body := strings.Repeat(base64.StdEncoding.EncodeToString([]byte("zzz")), size/3)
	return body + base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{'z'}, size%3))
}

func truncateTo(t *testing.T, path string, size int64) {
	t.Helper()
	file, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := file.Truncate(size); err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
}

func fsParamsOf(t *testing.T, path, dataBase64 string) jsontext.Value {
	t.Helper()
	members := map[string]string{"path": path}
	if dataBase64 != "" {
		members["dataBase64"] = dataBase64
	}
	raw, err := json.Marshal(members)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}
