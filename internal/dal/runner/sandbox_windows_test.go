//go:build windows

package runner

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"syscall"
	"testing"

	"golang.org/x/sys/windows"
)

func isolateWindowsSeed(t *testing.T) {
	t.Helper()
	t.Setenv("APPDATA", t.TempDir())
}

func runWindowsConfined(t *testing.T, roots []string, script string, confined bool) error {
	t.Helper()
	cmd := exec.Command("cmd")
	cmd.SysProcAttr = &syscall.SysProcAttr{CmdLine: `cmd /S /C "` + script + `"`}
	if confined {
		if err := confineWindowsCommand(cmd, roots); err != nil {
			t.Fatal(err)
		}
	}
	runErr := cmd.Run()
	releaseCommandToken(cmd)
	return runErr
}

func requireWindowsFile(t *testing.T, path string, want string) {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if got := strings.TrimSpace(string(data)); got != want {
		t.Fatalf("%s = %q, want %q", path, got, want)
	}
}

func requireWindowsAbsent(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("stat %s = %v, want not exist", path, err)
	}
}

func windowsACLEntryCount(t *testing.T, path string) int {
	t.Helper()
	handle, _, err := openACLTarget(path)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = windows.CloseHandle(handle) }()
	descriptor, err := windows.GetSecurityInfo(handle, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		t.Fatal(err)
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		t.Fatal(err)
	}
	return int(dacl.AceCount)
}

func TestWindowsConfineWritesOnlyUnderDeclaredRoots(t *testing.T) {
	isolateWindowsSeed(t)
	granted := t.TempDir()
	other := t.TempDir()
	script := `echo inside>"` + filepath.Join(granted, "inside.txt") + `" & echo outside>"` + filepath.Join(other, "outside.txt") + `"`
	if err := runWindowsConfined(t, []string{granted}, script, true); err == nil {
		t.Fatal("confined command exited 0 after a denied write")
	}
	requireWindowsFile(t, filepath.Join(granted, "inside.txt"), "inside")
	requireWindowsAbsent(t, filepath.Join(other, "outside.txt"))
	control := t.TempDir()
	controlScript := `echo inside>"` + filepath.Join(control, "inside.txt") + `" & echo outside>"` + filepath.Join(control, "outside.txt") + `"`
	if err := runWindowsConfined(t, nil, controlScript, false); err != nil {
		t.Fatalf("unconfined control: %v", err)
	}
	requireWindowsFile(t, filepath.Join(control, "outside.txt"), "outside")
}

func TestWindowsConfineWithoutRootsDeniesEveryWrite(t *testing.T) {
	isolateWindowsSeed(t)
	dir := t.TempDir()
	source := filepath.Join(dir, "source.txt")
	if err := os.WriteFile(source, []byte("kept"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := runWindowsConfined(t, nil, `type "`+source+`"`, true); err != nil {
		t.Fatalf("confined read: %v", err)
	}
	target := filepath.Join(dir, "target.txt")
	if err := runWindowsConfined(t, nil, `type "`+source+`">"`+target+`"`, true); err == nil {
		t.Fatal("confined command without roots exited 0 after a denied write")
	}
	requireWindowsAbsent(t, target)
	if err := runWindowsConfined(t, nil, `type "`+source+`">"`+target+`"`, false); err != nil {
		t.Fatalf("unconfined control: %v", err)
	}
	requireWindowsFile(t, target, "kept")
}

func TestWindowsGrantRootKeepsEntriesAndIsIdempotent(t *testing.T) {
	root := t.TempDir()
	first, err := windows.StringToSid("S-1-5-21-11-22-33-44")
	if err != nil {
		t.Fatal(err)
	}
	second, err := windows.StringToSid("S-1-5-21-55-66-77-88")
	if err != nil {
		t.Fatal(err)
	}
	before := windowsACLEntryCount(t, root)
	if err := grantRoot(root, first); err != nil {
		t.Fatal(err)
	}
	granted := windowsACLEntryCount(t, root)
	if granted <= before {
		t.Fatalf("entries after grant = %d, want more than %d", granted, before)
	}
	if err := grantRoot(root, first); err != nil {
		t.Fatal(err)
	}
	if again := windowsACLEntryCount(t, root); again != granted {
		t.Fatalf("entries after second grant = %d, want %d", again, granted)
	}
	if err := grantRoot(root, second); err != nil {
		t.Fatal(err)
	}
	if other := windowsACLEntryCount(t, root); other <= granted {
		t.Fatalf("entries after grant to a second SID = %d, want more than %d", other, granted)
	}
}

func TestWindowsGrantRootRefusesMissingRoot(t *testing.T) {
	sid, err := windows.StringToSid("S-1-5-21-11-22-33-44")
	if err != nil {
		t.Fatal(err)
	}
	if err := grantRoot(filepath.Join(t.TempDir(), "missing"), sid); err == nil {
		t.Fatal("grant to a missing root succeeded")
	}
}

func TestWindowsUnavailableCauseIsEmptyWhenTokensWork(t *testing.T) {
	if cause := UnavailableCause(); cause != "" {
		t.Fatalf("UnavailableCause = %q, want empty", cause)
	}
}

func TestWindowsCapabilitySIDDerivation(t *testing.T) {
	seed := bytes.Repeat([]byte{7}, capabilitySeedSize)
	other := bytes.Repeat([]byte{8}, capabilitySeedSize)
	shape := regexp.MustCompile(`^S-1-5-21(-[0-9]+){4}$`)
	first := deriveCapabilitySID(seed, rootCapabilityPrefix+`c:\work`)
	if !shape.MatchString(first) {
		t.Fatalf("SID %q does not match %s", first, shape)
	}
	if again := deriveCapabilitySID(seed, rootCapabilityPrefix+`c:\work`); again != first {
		t.Fatalf("derivation is not stable: %q then %q", first, again)
	}
	if deriveCapabilitySID(seed, rootCapabilityPrefix+`c:\work2`) == first {
		t.Fatal("two roots share one SID")
	}
	if deriveCapabilitySID(other, rootCapabilityPrefix+`c:\work`) == first {
		t.Fatal("two seeds share one SID")
	}
	if deriveCapabilitySID(seed, readOnlyCapabilityKey) == first {
		t.Fatal("the read-only SID equals a root SID")
	}
	if capabilityPathKey(`C:\Work\.\Sub`) != capabilityPathKey(`c:\work\sub`) {
		t.Fatal("path keys differ by case or dot segment")
	}
}

func TestWindowsReleaseCommandTokenClosesHandle(t *testing.T) {
	isolateWindowsSeed(t)
	cmd := exec.Command("cmd")
	if err := confineWindowsCommand(cmd, nil); err != nil {
		t.Fatal(err)
	}
	handle := windows.Handle(cmd.SysProcAttr.Token)
	if handle == 0 {
		t.Fatal("confinement set no token")
	}
	releaseCommandToken(cmd)
	if cmd.SysProcAttr.Token != 0 {
		t.Fatal("release left the token on the command")
	}
	if err := windows.CloseHandle(handle); !errors.Is(err, windows.ERROR_INVALID_HANDLE) {
		t.Fatalf("CloseHandle after release = %v, want invalid handle", err)
	}
	releaseCommandToken(cmd)
}

func TestWindowsShimConfinesWorkspaceWriteCommand(t *testing.T) {
	cache := t.TempDir()
	temp := t.TempDir()
	root := t.TempDir()
	other := t.TempDir()
	dir := t.TempDir()
	isolateWindowsSeed(t)
	t.Setenv("LOCALAPPDATA", cache)
	t.Setenv("TEMP", temp)
	t.Setenv("TMP", temp)
	t.Setenv("DALGONA_SANDBOX_MODE", string(SandboxWorkspaceWrite))
	t.Setenv("DALGONA_SANDBOX_ROOTS", encodeRoots([]string{root}))
	t.Setenv("DALGONA_RUN_CWD", root)
	script := `echo inside>"` + filepath.Join(root, "inside.txt") + `" & echo outside>"` + filepath.Join(other, "outside.txt") + `"`
	if err := runShim(dir, WindowsShellArgv(script)); err != nil {
		t.Fatal(err)
	}
	requireWindowsFile(t, filepath.Join(root, "inside.txt"), "inside")
	requireWindowsAbsent(t, filepath.Join(other, "outside.txt"))
	if record := readWindowsShimExit(t, dir); record.Code == 0 {
		t.Fatalf("exit record = %+v, want a nonzero code after the denied write", record)
	}
}

func TestWindowsReadOnlyIgnoresWritableRoots(t *testing.T) {
	roots, err := windowsWritableRoots(t.TempDir(), Sandbox{
		Mode:          SandboxMode("read-only"),
		WritableRoots: []string{t.TempDir()},
	})
	if err != nil || len(roots) != 0 {
		t.Fatalf("read-only roots = %v, %v", roots, err)
	}
}

func TestWindowsNetworkRefusalLeavesCommandUnstarted(t *testing.T) {
	cmd := exec.Command("cmd", "/C", "exit", "0")
	err := configureSandbox(cmd, t.TempDir(), Sandbox{
		Mode:          SandboxWorkspaceWrite,
		DeniedSockets: []string{filepath.Join(t.TempDir(), "daemon.sock")},
	})
	if err == nil || !strings.Contains(err.Error(), "network confinement is unavailable") {
		t.Fatalf("network refusal = %v", err)
	}
	if cmd.Process != nil || cmd.SysProcAttr != nil {
		t.Fatal("network refusal allocated a process or token")
	}
	if err := configureSandbox(cmd, "", Sandbox{Mode: SandboxOff}); err != nil {
		t.Fatalf("off control: %v", err)
	}
}
