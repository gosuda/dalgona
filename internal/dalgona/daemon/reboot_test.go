//go:build unix

package daemon

import (
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestDaemonReportsCommandLostToSimulatedReboot(t *testing.T) {
	interrupted := "interrupted: the host restarted while this command ran; it may have partly run"
	recoverAfterKill(t, recovery{command: "sleep 30", output: interrupted, recovered: "interrupted"}, killShimAndCommand)
}

func killShimAndCommand(t *testing.T, runDir string) {
	t.Helper()
	waitForPath(t, filepath.Join(runDir, "pgid"))
	shim := readFirstInt(t, filepath.Join(runDir, "pid"))
	group := readFirstInt(t, filepath.Join(runDir, "pgid"))
	for _, target := range []int{-group, shim} {
		if err := syscall.Kill(target, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
			t.Fatalf("kill %d: %v", target, err)
		}
	}
	deadline := time.Now().Add(5 * time.Second)
	for syscall.Kill(-group, 0) == nil || syscall.Kill(shim, 0) == nil {
		if time.Now().After(deadline) {
			t.Fatalf("shim %d or command group %d survived SIGKILL", shim, group)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func readFirstInt(t *testing.T, path string) int {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	fields := strings.Fields(string(data))
	if len(fields) == 0 {
		t.Fatalf("%s is empty", path)
	}
	value, err := strconv.Atoi(fields[0])
	if err != nil {
		t.Fatalf("%s: %v", path, err)
	}
	return value
}
