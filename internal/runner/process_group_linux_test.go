//go:build linux

package runner

import (
	"bufio"
	"os/exec"
	"syscall"
	"testing"

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
	cmd := exec.Command("sh", "-c", "echo ready; exec sleep 30")
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	pgid := cmd.Process.Pid
	defer func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	}()
	ready, err := bufio.NewReader(out).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	if ready != "ready\n" {
		t.Fatalf("child said %q, want ready", ready)
	}
	if processGroupStopped(pgid) {
		t.Fatal("processGroupStopped live group = true, want false")
	}
	if processGroupDrained(pgid) {
		t.Fatal("processGroupDrained live group = true, want false")
	}
}
