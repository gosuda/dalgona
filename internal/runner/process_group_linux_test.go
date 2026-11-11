//go:build linux

package runner

import (
	"os/exec"
	"syscall"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func TestProcessGroupDrainedZombieOnly(t *testing.T) {
	cmd := exec.Command("true")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	var exited unix.Siginfo
	if err := unix.Waitid(unix.P_PID, pgid, &exited, unix.WEXITED|unix.WNOWAIT, nil); err != nil {
		t.Fatalf("wait for zombie child = %v, want nil", err)
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
