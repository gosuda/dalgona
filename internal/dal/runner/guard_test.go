//go:build !windows

package runner

import (
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/shellargs"
)

const (
	guardDaemonPID = 4242
	guardParentPID = 4243
)

func fakeGuardLabels(pid int) []string {
	switch pid {
	case guardDaemonPID:
		return []string{"fakedaemon-bin", "fakedaemon-bin --serve"}
	case guardParentPID:
		return []string{"fakeparent"}
	}
	return nil
}

func testGuard() killGuard {
	return killGuard{daemonPID: guardDaemonPID, parentPID: guardParentPID, labels: fakeGuardLabels}
}

func nestShell(levels int, inner string) string {
	command := inner
	for range levels {
		command = "sh -c " + shellargs.QuoteToken(command)
	}
	return command
}

func TestProcessKillGuard(t *testing.T) {
	refused := []string{
		"kill -1",
		"kill 0",
		"kill -9 0",
		"kill -KILL 0",
		"kill -- -1",
		"kill -9 -1",
		"kill -s KILL -1",
		"kill -n 9 -1",
		"kill --signal=KILL -1",
		"kill 4242",
		"kill 4243",
		"kill -9 4242",
		"kill -0 4242",
		"kill -s TERM 4243",
		"kill 4300 4242",
		"kill 004242",
		"kill 4294971538",
		"/bin/kill 4242",
		"sudo kill 4242",
		"sudo -u root -E kill 4242",
		"env A=1 kill 4242",
		"timeout 5 kill 4242",
		"nice -n 5 kill 4242",
		"exec kill 4242",
		"command kill 4242",
		"nohup time -p kill 4242",
		`\kill 4242`,
		`"kill" 4242`,
		"kill $PPID",
		"kill $$",
		"kill ${PPID}",
		`kill "$PPID"`,
		"kill $(pidof dalgona)",
		"kill `pidof dalgona`",
		"kill %1",
		"kill +1x",
		"kill",
		"kill -9",
		"pkill dalgona",
		"pkill dal",
		"pkill -9 -x dalgona",
		"pkill -u root dalgona",
		"killall dalgona",
		"killall dal",
		"killall ''",
		"pkill ''",
		"pkill fakedaemon-bin",
		"pkill fakeparent",
		"killall fakeparent",
		"pkill -f 'fakedaemon-bin --serve'",
		"killall -r 'fake.*'",
		"pkill -u alice",
		"pkill -v unrelated-name-for-test",
		"pkill '('",
		"pkill",
		"echo ok; kill 4242",
		"true && kill 4242",
		"true || kill 4242",
		"true | kill 4242",
		"if true; then kill 4242; fi",
		"(kill 4242)",
		"{ kill 4242; }",
		"f() { kill 4242; }; f",
		"bash -c 'kill 4242'",
		`sh -lc "kill 4242"`,
		"bash -o pipefail -c 'kill 4242'",
		`bash -c "bash -c 'kill 4242'"`,
		"zsh -c 'kill $PPID'",
		"eval kill 4242",
		`eval "kill \$PPID"`,
		`echo "$(kill 4242)"`,
		"echo `kill 4242`",
		`echo "'" "$(kill 4242)"`,
		"xargs kill",
		"ps -o pid= | xargs kill -9",
		"xargs -n1 sudo kill",
		"xargs -I{} kill {}",
		"xargs pkill",
		"find . -exec kill {} \\;",
		"find . -name x -exec echo {} \\; -exec kill {} \\;",
		"find /proc -execdir sh -c 'kill $PPID' sh {} +",
		"sudo -i kill 4242",
		"env -S 'kill 4242'",
		`echo "unterminated; kill 4242`,
	}
	allowed := []string{
		"kill -0 4300",
		"kill 4300",
		"kill -9 4300 4301",
		"kill -s TERM 4300",
		"kill -TERM 4300",
		"kill -l",
		"kill -l 9",
		"kill --help",
		"echo kill -9 4242",
		`echo "kill 4242"`,
		"echo 'kill $PPID'",
		"printf '%s\\n' 'pkill dalgona'",
		"grep kill file.txt",
		"git log --grep kill",
		`echo "$(date)"`,
		"echo '$(kill 4242)'",
		"pkill unrelated-name-for-test",
		"killall unrelated-name-for-test",
		"pkill -f 'unrelated.*server'",
		"pkill -u alice unrelated-name-for-test",
		"killall -r 'unrelated.*'",
		"sudo ls",
		"sudo -u skillet ls",
		"env A=1 ls",
		"timeout 5 sleep 1",
		"nice -n 5 sleep 1",
		"xargs echo",
		"find . -name x -exec echo {} \\;",
		"bash -c 'echo hi'",
		"bash -o pipefail -c 'echo hi'",
		"sh script.sh",
		"eval echo hi",
		"if true; then echo ok; fi",
		`echo "unterminated`,
	}
	guard := testGuard()
	for _, command := range refused {
		t.Run("refused/"+command, func(t *testing.T) {
			if !guard.refusesTokens([]string{"sh", "-c", command}, 0) {
				t.Fatalf("guard allowed %q", command)
			}
		})
	}
	for _, command := range allowed {
		t.Run("allowed/"+command, func(t *testing.T) {
			if guard.refusesTokens([]string{"sh", "-c", command}, 0) {
				t.Fatalf("guard refused %q", command)
			}
		})
	}
}

func TestProcessKillGuardBoundsRecursionAtEightLevels(t *testing.T) {
	guard := testGuard()
	rows := []struct {
		name    string
		command string
		refused bool
	}{
		{"eight levels of a harmless command", nestShell(8, "true"), false},
		{"eight levels of echoed kill text", nestShell(8, "echo kill 4242"), false},
		{"eight levels reach the kill", nestShell(8, "kill 4242"), true},
		{"nine levels are ambiguous", nestShell(9, "true"), true},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			if got := guard.refusesCommand(row.command, 0); got != row.refused {
				t.Fatalf("refusesCommand = %v, want %v", got, row.refused)
			}
		})
	}
}

func TestCheckProcessKillGuard(t *testing.T) {
	refused := [][]string{
		{"kill", "4242"},
		{"sudo", "kill", "4243"},
		{"env", "A=1", "pkill", "dalgona"},
		{"sh", "-c", "kill 4242"},
		{"kill", "-1"},
	}
	for _, argv := range refused {
		err := CheckProcessKillGuard(argv, guardDaemonPID, guardParentPID)
		if !errors.Is(err, ErrProcessKillGuard) {
			t.Errorf("CheckProcessKillGuard(%q) = %v, want ErrProcessKillGuard", argv, err)
			continue
		}
		if !strings.HasPrefix(err.Error(), ErrProcessKillGuard.Error()) {
			t.Errorf("error %q does not start with the sentinel text", err)
		}
	}
	allowed := [][]string{
		nil,
		{},
		{"kill", "4300"},
		{"kill", "-0", "4300"},
		{"echo", "kill", "4242"},
		{"sh", "-c", "echo kill 4242"},
		{"@self", "__self-test"},
		{"sh", "-c", "sleep 1"},
	}
	for _, argv := range allowed {
		if err := CheckProcessKillGuard(argv, guardDaemonPID, guardParentPID); err != nil {
			t.Errorf("CheckProcessKillGuard(%q) = %v, want nil", argv, err)
		}
	}
}

func TestProcessKillGuardMatchesRunningBinaryName(t *testing.T) {
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	name := regexp.QuoteMeta(filepath.Base(exe))
	pids := [2]int{guardDaemonPID, guardParentPID}
	if err := CheckProcessKillGuard([]string{"pkill", name}, pids[0], pids[1]); !errors.Is(err, ErrProcessKillGuard) {
		t.Fatalf("pkill of the running binary name = %v, want ErrProcessKillGuard", err)
	}
	if err := CheckProcessKillGuard([]string{"pkill", name + "-suffix-for-test"}, pids[0], pids[1]); err != nil {
		t.Fatalf("pkill of an unrelated name = %v, want nil", err)
	}
}

func TestProcessKillGuardMatchesParentProcessName(t *testing.T) {
	labels := processLabels(os.Getppid())
	if len(labels) == 0 || labels[0] == "" {
		t.Skip("process names are not readable on this platform")
	}
	name := regexp.QuoteMeta(labels[0])
	if err := CheckProcessKillGuard([]string{"pkill", name}, os.Getpid(), os.Getppid()); !errors.Is(err, ErrProcessKillGuard) {
		t.Fatalf("pkill of the parent name %q = %v, want ErrProcessKillGuard", labels[0], err)
	}
	if err := CheckProcessKillGuard([]string{"pkill", name + "-suffix-for-test"}, os.Getpid(), os.Getppid()); err != nil {
		t.Fatalf("pkill of an unrelated name = %v, want nil", err)
	}
}

func TestNewLocalProtectsDaemonAndParent(t *testing.T) {
	local := newTestLocal(t, filepath.Join(t.TempDir(), "runs"))
	if want := [2]int{os.Getpid(), os.Getppid()}; local.protectedPIDs != want {
		t.Fatalf("protectedPIDs = %v, want %v", local.protectedPIDs, want)
	}
}

func TestLocalStartRefusesProtectedKill(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	marker := filepath.Join(dir, "marker")
	own := strconv.Itoa(os.Getpid())
	parent := strconv.Itoa(os.Getppid())
	for _, command := range []string{
		"touch " + marker + "; kill -0 " + own,
		"touch " + marker + "; kill -0 " + parent,
		"touch " + marker + "; kill -0 $PPID",
		"touch " + marker + "; kill -0 -1",
		"touch " + marker + "; pkill -0 dalgona",
	} {
		err := local.Start(t.Context(), ProcessSpec{ID: "refused", Argv: []string{"sh", "-c", command}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
		if !errors.Is(err, ErrProcessKillGuard) {
			t.Fatalf("Start(%q) = %v, want ErrProcessKillGuard", command, err)
		}
		entries, readErr := os.ReadDir(local.dir)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if len(entries) != 0 {
			t.Fatalf("Start(%q) created call directories: %v", command, entries)
		}
		if _, statErr := os.Stat(marker); !errors.Is(statErr, os.ErrNotExist) {
			t.Fatalf("Start(%q) ran the command: %v", command, statErr)
		}
	}
}

func TestLocalStartUsesItsProtectedPIDs(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	local.protectedPIDs = [2]int{guardDaemonPID, guardParentPID}
	err := local.Start(t.Context(), ProcessSpec{ID: "refused", Argv: []string{"sh", "-c", "kill -0 " + strconv.Itoa(guardParentPID)}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	if !errors.Is(err, ErrProcessKillGuard) {
		t.Fatalf("Start of a kill of the configured parent = %v, want ErrProcessKillGuard", err)
	}
}

func TestLocalStartRunsEchoedKillText(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	command := "echo kill -9 " + strconv.Itoa(os.Getpid()) + "; touch " + filepath.Join(dir, "marker")
	startTestProcess(t, local, ProcessSpec{ID: "echoed", Argv: []string{"sh", "-c", command}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	state := waitForExit(t, local, "echoed")
	if state.Exit.Code != 0 {
		t.Fatalf("exit code = %d, want 0", state.Exit.Code)
	}
	if want := "kill -9 " + strconv.Itoa(os.Getpid()) + "\n"; string(state.Data) != want {
		t.Fatalf("output = %q, want %q", state.Data, want)
	}
	if _, err := os.Stat(filepath.Join(dir, "marker")); err != nil {
		t.Fatalf("command did not finish: %v", err)
	}
}
