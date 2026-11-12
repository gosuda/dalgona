//go:build linux

package runner

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLocal_shimRunsSameBuildAsParent(t *testing.T) {
	if os.Getenv("DALGONA_SAME_BUILD_HELPER") == "1" {
		runSameBuildHelper(t)
		return
	}
	dir := t.TempDir()
	helper := copyTestBinary(t, dir)
	script := writeReplaceScript(t, dir)
	ready := filepath.Join(dir, "ready")
	replaced := filepath.Join(dir, "replaced")
	cmd, stdout, stderr := startSameBuildHelper(t, helper, ready, replaced)
	waitForPath(t, ready, 20*time.Second)
	if err := os.Rename(script, helper); err != nil {
		t.Fatalf("replace helper copy: %v", err)
	}
	if err := os.WriteFile(replaced, nil, 0o644); err != nil {
		t.Fatalf("signal replaced: %v", err)
	}
	waitHelperExit(t, cmd, stdout, stderr)
}

func copyTestBinary(t *testing.T, dir string) string {
	t.Helper()
	self, err := os.Executable()
	if err != nil {
		t.Fatalf("resolve test binary: %v", err)
	}
	helper := filepath.Join(dir, "helper")
	data, err := os.ReadFile(self)
	if err != nil {
		t.Fatalf("read test binary: %v", err)
	}
	if err := os.WriteFile(helper, data, 0o755); err != nil {
		t.Fatalf("copy test binary: %v", err)
	}
	return helper
}

func writeReplaceScript(t *testing.T, dir string) string {
	t.Helper()
	script := filepath.Join(dir, "script")
	if err := os.WriteFile(script, []byte("#!/bin/sh\necho SCRIPT-RAN\n"), 0o755); err != nil {
		t.Fatalf("write script: %v", err)
	}
	return script
}

func startSameBuildHelper(t *testing.T, helper string, ready string, replaced string) (*exec.Cmd, *strings.Builder, *strings.Builder) {
	t.Helper()
	cmd := exec.Command(helper, "-test.run=TestLocal_shimRunsSameBuildAsParent", "-test.v")
	cmd.Env = append(os.Environ(),
		"DALGONA_SAME_BUILD_HELPER=1",
		"DALGONA_SAME_BUILD_READY="+ready,
		"DALGONA_SAME_BUILD_REPLACED="+replaced,
	)
	cmd.Dir = filepath.Dir(helper)
	stdout := &strings.Builder{}
	stderr := &strings.Builder{}
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start helper: %v", err)
	}
	return cmd, stdout, stderr
}

func waitHelperExit(t *testing.T, cmd *exec.Cmd, stdout *strings.Builder, stderr *strings.Builder) {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	timer := time.NewTimer(60 * time.Second)
	defer timer.Stop()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("helper failed: %v stdout %s stderr %s", err, stdout.String(), stderr.String())
		}
	case <-timer.C:
		_ = cmd.Process.Kill()
		err := <-done
		t.Fatalf("helper timed out: last error %v stdout %s stderr %s", err, stdout.String(), stderr.String())
	}
}

func waitForPath(t *testing.T, path string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(path); err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("%s never appeared", path)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func runSameBuildHelper(t *testing.T) {
	ready := os.Getenv("DALGONA_SAME_BUILD_READY")
	replaced := os.Getenv("DALGONA_SAME_BUILD_REPLACED")
	if err := os.WriteFile(ready, nil, 0o644); err != nil {
		t.Fatalf("signal ready: %v", err)
	}
	deadline := time.Now().Add(20 * time.Second)
	for {
		if _, err := os.Stat(replaced); err == nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("parent never replaced the helper copy")
		}
		time.Sleep(10 * time.Millisecond)
	}
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	startTestProcess(t, local, ProcessSpec{
		ID:   "samebuild",
		Argv: []string{"@self", "__self-test"},
		Dir:  dir,
		Env:  []string{"PATH=" + os.Getenv("PATH"), "HOME=" + dir},
	})
	state := waitForExit(t, local, "samebuild")
	if state.Exit == nil || state.Exit.Code != 0 {
		t.Fatalf("call did not end with exit code 0: %+v output %s", state.Exit, state.Data)
	}
	if strings.Contains(string(state.Data), "SCRIPT-RAN") {
		t.Fatalf("the replaced script ran instead of the parent build: %s", state.Data)
	}
}
