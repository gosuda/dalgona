//go:build !windows

package runner

import (
	"os"
	"os/exec"
	"testing"
	"time"
)

func TestSignalProcessMissingGroup(t *testing.T) {
	if err := signalProcess(os.Getpid(), 1<<30, SignalKill); err != nil {
		t.Fatalf("signalProcess missing group = %v, want nil", err)
	}
	if !processGroupGone(1 << 30) {
		t.Fatal("processGroupGone missing group = false, want true")
	}
}

func TestSignalProcessLiveGroup(t *testing.T) {
	cmd := exec.Command("sh", "-c", "sleep 30")
	configureProcessGroup(cmd)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	cleanup := func() {
		_ = signalProcess(os.Getpid(), pgid, SignalKill)
		_ = cmd.Wait()
	}
	defer cleanup()
	if processGroupGone(pgid) {
		t.Fatal("processGroupGone live group = true, want false")
	}
	if err := signalProcess(os.Getpid(), pgid, SignalKill); err != nil {
		t.Fatalf("signalProcess live group = %v, want nil", err)
	}
	_ = cmd.Wait()
	deadline := time.Now().Add(5 * time.Second)
	for !processGroupGone(pgid) {
		if time.Now().After(deadline) {
			t.Fatal("processGroupGone killed group = false after 5s, want true")
		}
		time.Sleep(10 * time.Millisecond)
	}
}
