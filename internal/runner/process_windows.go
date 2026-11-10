//go:build windows

package runner

import (
	"context"
	"errors"
	"fmt"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	jobObjectTerminate = 0x0008
	stillActive        = 259
)

func startDetached(ctx context.Context, self string, args []string, spec ProcessSpec) (*exec.Cmd, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: detached start: %w", err)
	}
	cmd := exec.Command(self, args...)
	cmd.Dir = spec.Dir
	cmd.Env = spec.Env
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: windows.DETACHED_PROCESS | windows.CREATE_NEW_PROCESS_GROUP}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("runner: start shim: %w", err)
	}
	return cmd, nil
}

func configureProcessGroup(cmd *exec.Cmd) {}

func configureShimCommand(cmd *exec.Cmd, argv []string) {
	command, ok := splitWindowsShellCommand(argv)
	if !ok {
		return
	}
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	shape := WindowsShellArgv("")
	cmd.SysProcAttr.CmdLine = shape[0] + " /S " + shape[1] + ` "` + command + `"`
}

func splitWindowsShellCommand(argv []string) (string, bool) {
	shape := WindowsShellArgv("")
	if len(argv) != len(shape) {
		return "", false
	}
	base := strings.ToLower(filepath.Base(argv[0]))
	shell := strings.ToLower(shape[0])
	if base != shell && base != shell+".exe" {
		return "", false
	}
	if !strings.EqualFold(argv[1], shape[1]) {
		return "", false
	}
	return argv[2], true
}

func signalProcess(shimPID int, pgid int, signal Signal) error {
	job, err := openJobObject(jobName(shimPID))
	if err != nil {
		return err
	}
	defer func() { _ = windows.CloseHandle(job) }()
	if err := windows.TerminateJobObject(job, jobExitCode(signal)); err != nil {
		return fmt.Errorf("runner: terminate job: %w", err)
	}
	return nil
}

func ensureProcessGroupGone(pgid int, signal Signal) {}

func watchCancelWhileWaiting(dir string, pgid int, done <-chan struct{}) {}

func jobExitCode(signal Signal) uint32 {
	if signal == SignalKill {
		return 137
	}
	return 1
}

func canceledExit(signal Signal) (int, string) {
	return int(jobExitCode(signal)), ""
}

func windowsPID(pid int) (uint32, bool) {
	if pid < 0 || uint64(pid) > math.MaxUint32 {
		return 0, false
	}
	return uint32(pid), true
}

func assignJob(cmd *exec.Cmd) (func(), error) {
	name, err := windows.UTF16PtrFromString(jobName(os.Getpid()))
	if err != nil {
		return nil, fmt.Errorf("runner: encode job name: %w", err)
	}
	job, err := windows.CreateJobObject(nil, name)
	if err != nil {
		return nil, fmt.Errorf("runner: create job: %w", err)
	}
	pid, ok := windowsPID(cmd.Process.Pid)
	if !ok {
		_ = windows.CloseHandle(job)
		return nil, fmt.Errorf("runner: invalid command pid %d", cmd.Process.Pid)
	}
	process, err := windows.OpenProcess(windows.PROCESS_SET_QUOTA|windows.PROCESS_TERMINATE, false, pid)
	if err != nil {
		_ = windows.CloseHandle(job)
		return nil, fmt.Errorf("runner: open command process: %w", err)
	}
	if err := windows.AssignProcessToJobObject(job, process); err != nil {
		_ = windows.CloseHandle(process)
		_ = windows.CloseHandle(job)
		return nil, fmt.Errorf("runner: assign command job: %w", err)
	}
	cleanup := func() {
		_ = windows.CloseHandle(process)
		_ = windows.CloseHandle(job)
	}
	return cleanup, nil
}

func openJobObject(name string) (windows.Handle, error) {
	encoded, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return 0, fmt.Errorf("runner: encode job name: %w", err)
	}
	procedure := windows.NewLazySystemDLL("kernel32.dll").NewProc("OpenJobObjectW")
	handle, _, callErr := procedure.Call(jobObjectTerminate, 0, uintptr(unsafe.Pointer(encoded))) //nolint:gosec // OpenJobObjectW expects the job name passed as uintptr
	if handle == 0 {
		return 0, fmt.Errorf("runner: open job: %w", callErr)
	}
	return windows.Handle(handle), nil
}

func jobName(pid int) string {
	return `Local\dalgona-shim-` + strconv.Itoa(pid)
}

func processIsAlive(pid int) bool {
	wpid, ok := windowsPID(pid)
	if !ok {
		return false
	}
	process, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, wpid)
	if err != nil {
		return false
	}
	defer func() { _ = windows.CloseHandle(process) }()
	var code uint32
	if err := windows.GetExitCodeProcess(process, &code); err != nil {
		return false
	}
	return code == stillActive
}

func processStartTime(pid int) string {
	wpid, ok := windowsPID(pid)
	if !ok {
		return ""
	}
	process, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, wpid)
	if err != nil {
		return ""
	}
	defer func() { _ = windows.CloseHandle(process) }()
	var creation windows.Filetime
	var exit windows.Filetime
	var kernel windows.Filetime
	var user windows.Filetime
	if err := windows.GetProcessTimes(process, &creation, &exit, &kernel, &user); err != nil {
		return ""
	}
	value := uint64(creation.HighDateTime)<<32 | uint64(creation.LowDateTime)
	return strconv.FormatUint(value, 10)
}

func exitDetails(waitErr error) (int, string) {
	if waitErr == nil {
		return 0, ""
	}
	exitErr, ok := errors.AsType[*exec.ExitError](waitErr)
	if !ok {
		return 1, ""
	}
	return exitErr.ExitCode(), ""
}

func syncDir(path string) error { return nil }
