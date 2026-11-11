//go:build linux

package runner

import (
	"io"
	"os/exec"
	"syscall"
	"testing"
	"time"
)

func TestProcessGroupDrainedZombieOnly(t *testing.T) {
	cmd := exec.Command("true")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	if _, err := io.ReadAll(out); err != nil {
		t.Fatalf("zombie child output = %v, want EOF", err)
	}
	if processGroupGone(pgid) {
		t.Fatal("processGroupGone zombie-only group = true, want false")
	}
	if !processGroupDrained(pgid) {
		t.Fatal("processGroupDrained zombie-only group = false, want true")
	}
	if waitErr := cmd.Wait(); waitErr != nil {
		t.Fatalf("zombie reap = %v, want nil", waitErr)
	}
	if !processGroupGone(pgid) {
		t.Fatal("processGroupGone reaped group = false, want true")
	}
}

func TestProcessGroupDrainedLiveGroup(t *testing.T) {
	cmd := exec.Command("sleep", "30")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	defer func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	}()
	deadline := time.Now().Add(10 * time.Second)
	for processGroupStopped(pgid) {
		if time.Now().After(deadline) {
			t.Fatalf("process group %d stopped before it started", pgid)
		}
		time.Sleep(5 * time.Millisecond)
	}
	if processGroupDrained(pgid) {
		t.Fatal("processGroupDrained live group = true, want false")
	}
}
