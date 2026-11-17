//go:build !windows

package runner

import (
	"errors"
	"os"
	"os/exec"
	"syscall"
	"testing"
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
	cmd := exec.Command("sleep", "30")
	configureProcessGroup(cmd)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	defer func() {
		_ = signalProcess(os.Getpid(), pgid, SignalKill)
		_ = cmd.Wait()
	}()
	if processGroupGone(pgid) {
		t.Fatal("processGroupGone live group = true, want false")
	}
	if err := signalProcess(os.Getpid(), pgid, SignalKill); err != nil {
		t.Fatalf("signalProcess live group = %v, want nil", err)
	}
	waitErr := cmd.Wait()
	exitErr, ok := errors.AsType[*exec.ExitError](waitErr)
	if !ok {
		t.Fatalf("live wait = %v, want signaled exit", waitErr)
	}
	status, ok := exitErr.Sys().(syscall.WaitStatus)
	if !ok {
		t.Fatalf("live status type = %T, want WaitStatus", exitErr.Sys())
	}
	if !status.Signaled() || status.Signal() != syscall.SIGKILL {
		t.Fatalf("live status = %v, want SIGKILL", status)
	}
	if !processGroupGone(pgid) {
		t.Fatal("processGroupGone killed group = false, want true")
	}
}
