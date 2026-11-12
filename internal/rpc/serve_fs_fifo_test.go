//go:build !windows

package rpc

import (
	"path/filepath"
	"syscall"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestFsReadFileRefusesFIFO(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	fifo := filepath.Join(t.TempDir(), "pipe")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	c.fails("fs/readFile", map[string]any{"path": fifo}, -32602, "not a regular file: "+fifo)
}
