//go:build unix

package client

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestSSHLinkOutlivesCanceledSetupAndReapsChild(t *testing.T) {
	machine := strings.Repeat("a", 64)
	fixture := writeSSHIdentity(t, machine)
	cmd := exec.Command("sh", "-c", `cat "$1"; exec sleep 60`, "ssh-fixture", fixture)
	stream, err := startSSH(cmd)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = stream.Close() })
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	if got, err := sshIdentify(ctx, stream, machine); err != nil || got != machine {
		t.Fatalf("SSH setup did not bind machine: %v", err)
	}
	cancel()
	if err := stream.process.Signal(syscall.Signal(0)); err != nil {
		t.Fatalf("setup cancellation killed active SSH child: %v", err)
	}
	if err := stream.Close(); err != nil {
		t.Fatal(err)
	}
	if err := stream.process.Signal(syscall.Signal(0)); !errors.Is(err, os.ErrProcessDone) {
		t.Fatalf("SSH close did not reap owned child: %v", err)
	}
}

func writeSSHIdentity(t *testing.T, machine string) string {
	t.Helper()
	var data bytes.Buffer
	if err := rpc.WriteMachineFrame(&data, rpc.MachineReply{Version: rpc.MachineVersion, MachineID: machine}); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "identity")
	if err := os.WriteFile(path, data.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestPOSIXSSHInstallRefusesModifiedBytesBeforePublication(t *testing.T) {
	home := t.TempDir()
	artifact := []byte("trusted artifact bytes\n")
	digest := sha256.Sum256(artifact)
	selector := hex.EncodeToString(digest[:])
	installed := filepath.Join(home, ".dalgona-bin", "dalgona-"+selector)
	if err := runPOSIXInstaller(t, home, selector, []byte("changed during transfer")); err == nil {
		t.Fatal("installer accepted an artifact with the wrong digest")
	}
	if _, err := os.Stat(installed); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("bad transfer published an executable: %v", err)
	}
	if err := runPOSIXInstaller(t, home, selector, artifact); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(installed)
	if err != nil || !bytes.Equal(got, artifact) {
		t.Fatalf("verified artifact was not published unchanged: %v", err)
	}
	info, err := os.Stat(installed)
	if err != nil || info.Mode().Perm() != 0o700 {
		t.Fatalf("installed artifact was not private and executable: %v", err)
	}
}

func runPOSIXInstaller(t *testing.T, home, digest string, payload []byte) error {
	t.Helper()
	cmd := exec.CommandContext(t.Context(), "sh", "-c", posixInstall(digest))
	cmd.Env = append(cmd.Environ(), "HOME="+home)
	cmd.Stdin = bytes.NewReader(payload)
	return cmd.Run()
}
