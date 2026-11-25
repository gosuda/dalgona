package figure

import (
	"bufio"
	"io"
	"os"
	"os/exec"
	"testing"
	"time"
)

const residentChildBytes = 96 << 20

var residentChildHeap []byte

func residentChild() int {
	residentChildHeap = make([]byte, residentChildBytes)
	for i := 0; i < len(residentChildHeap); i += 4096 {
		residentChildHeap[i] = 1
	}
	if _, err := os.Stdout.WriteString("ready\n"); err != nil {
		return 1
	}
	_, _ = io.Copy(io.Discard, os.Stdin)
	return 0
}

func TestResidentBytesSamplesAChildProcess(t *testing.T) {
	cmd := startResidentChild(t)
	sampled := awaitResident(t, cmd.Process.Pid)
	if sampled < residentChildBytes || sampled > ChildMemoryLimit {
		t.Fatalf("child resident memory = %d bytes, want between %d touched bytes and %d", sampled, residentChildBytes, ChildMemoryLimit)
	}
	if _, ok := residentBytes(-1); ok {
		t.Fatal("an impossible process identifier reports resident memory")
	}
}

func startResidentChild(t *testing.T) *exec.Cmd {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe, testChildResident)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = stdin.Close()
		_ = cmd.Wait()
	})
	if line, err := bufio.NewReader(stdout).ReadString('\n'); err != nil || line != "ready\n" {
		t.Fatalf("resident child said %q, %v", line, err)
	}
	return cmd
}

func awaitResident(t *testing.T, pid int) int64 {
	t.Helper()
	var sampled int64
	for deadline := time.Now().Add(30 * time.Second); time.Now().Before(deadline); time.Sleep(MemorySamplePeriod) {
		rss, ok := residentBytes(pid)
		if !ok {
			t.Fatal("resident memory of a live child could not be sampled")
		}
		if sampled = rss; sampled >= residentChildBytes {
			break
		}
	}
	return sampled
}
