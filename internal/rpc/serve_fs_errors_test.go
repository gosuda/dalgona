package rpc

import (
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestFsWriteFileMissingParentIsNoSuchFile(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	path := filepath.Join(t.TempDir(), "missing", "file.txt")
	c.fails("fs/writeFile", map[string]any{"path": path, "dataBase64": "eA=="}, -32602, "no such file: "+path)
}
