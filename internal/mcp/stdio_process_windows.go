//go:build windows

package mcp

import (
	"errors"
	"fmt"
	"math"
	"os"
	"os/exec"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

func configureStdioProcess(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{
		CreationFlags: windows.CREATE_NEW_PROCESS_GROUP | windows.CREATE_SUSPENDED,
	}
	cmd.WaitDelay = stdioWaitDelay
}

const stdioProcessExitWait = 5000

func killStdioProcess(cmd *exec.Cmd) error {
	proc := cmd.Process
	if proc == nil {
		return nil
	}
	if err := proc.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
		if stdioProcessExited(proc.Pid) {
			return nil
		}
		return err
	}
	return nil
}

func stdioProcessExited(pid int) bool {
	pid32, ok := windowsPID(pid)
	if !ok {
		return false
	}
	handle, err := windows.OpenProcess(windows.SYNCHRONIZE|windows.PROCESS_QUERY_LIMITED_INFORMATION, false, pid32)
	if err != nil {
		return errors.Is(err, windows.ERROR_INVALID_PARAMETER)
	}
	defer func() { _ = windows.CloseHandle(handle) }()
	event, err := windows.WaitForSingleObject(handle, stdioProcessExitWait)
	if err != nil {
		return false
	}
	return event == windows.WAIT_OBJECT_0
}

func windowsPID(pid int) (uint32, bool) {
	if pid < 0 || uint64(pid) > math.MaxUint32 {
		return 0, false
	}
	return uint32(pid), true
}

func assignStdioJob(cmd *exec.Cmd) (func(), error) {
	if cmd.Process == nil {
		return nil, errors.New("mcp: stdio process not started")
	}
	pid, ok := windowsPID(cmd.Process.Pid)
	if !ok {
		return nil, fmt.Errorf("mcp: invalid stdio process pid %d", cmd.Process.Pid)
	}
	job, err := windows.CreateJobObject(nil, nil)
	if err != nil {
		resumeStdioProcessBestEffort(pid)
		return nil, fmt.Errorf("mcp: create stdio job: %w", err)
	}
	killErr := setKillOnJobClose(job)
	if killErr != nil {
		_ = windows.CloseHandle(job)
		resumeStdioProcessBestEffort(pid)
		return nil, fmt.Errorf("mcp: limit stdio job: %w", killErr)
	}
	process, err := windows.OpenProcess(windows.PROCESS_SET_QUOTA|windows.PROCESS_TERMINATE, false, pid)
	if err != nil {
		_ = windows.CloseHandle(job)
		resumeStdioProcessBestEffort(pid)
		return nil, fmt.Errorf("mcp: open stdio process: %w", err)
	}
	if err := windows.AssignProcessToJobObject(job, process); err != nil {
		_ = windows.CloseHandle(process)
		_ = windows.CloseHandle(job)
		resumeStdioProcessBestEffort(pid)
		return nil, fmt.Errorf("mcp: assign stdio job: %w", err)
	}
	if err := resumeStdioProcess(pid); err != nil {
		_ = windows.TerminateJobObject(job, 137)
		_ = windows.CloseHandle(process)
		_ = windows.CloseHandle(job)
		return nil, err
	}
	cleanup := func() {
		_ = windows.TerminateJobObject(job, 137)
		_ = windows.CloseHandle(process)
		_ = windows.CloseHandle(job)
	}
	return cleanup, nil
}

func setKillOnJobClose(job windows.Handle) error {
	var info windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION
	info.BasicLimitInformation.LimitFlags = windows.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
	_, err := windows.SetInformationJobObject(
		job,
		windows.JobObjectExtendedLimitInformation,
		uintptr(unsafe.Pointer(&info)), //nolint:gosec // SetInformationJobObject expects a pointer to the limit struct
		uint32(unsafe.Sizeof(info)),
	)
	return err
}

func resumeStdioProcess(pid uint32) error {
	var lastErr error
	for range 20 {
		resumed, err := resumeStdioThreadsOnce(pid)
		if resumed {
			return nil
		}
		if err != nil {
			lastErr = err
		} else {
			lastErr = errors.New("mcp: no stdio threads to resume")
		}
		time.Sleep(5 * time.Millisecond)
	}
	return fmt.Errorf("mcp: resume stdio process: %w", lastErr)
}

func resumeStdioThreadsOnce(pid uint32) (bool, error) {
	snapshot, err := windows.CreateToolhelp32Snapshot(windows.TH32CS_SNAPTHREAD, 0)
	if err != nil {
		return false, fmt.Errorf("mcp: snapshot stdio threads: %w", err)
	}
	defer func() { _ = windows.CloseHandle(snapshot) }()
	var entry windows.ThreadEntry32
	entry.Size = uint32(unsafe.Sizeof(entry))
	if err := windows.Thread32First(snapshot, &entry); err != nil {
		return false, fmt.Errorf("mcp: list stdio threads: %w", err)
	}
	resumed := false
	for {
		if entry.OwnerProcessID == pid {
			thread, err := windows.OpenThread(windows.THREAD_SUSPEND_RESUME, false, entry.ThreadID)
			if err != nil {
				return resumed, fmt.Errorf("mcp: open stdio thread: %w", err)
			}
			_, err = windows.ResumeThread(thread)
			_ = windows.CloseHandle(thread)
			if err != nil {
				return resumed, fmt.Errorf("mcp: resume stdio thread: %w", err)
			}
			resumed = true
		}
		if err := windows.Thread32Next(snapshot, &entry); err != nil {
			break
		}
	}
	return resumed, nil
}

func resumeStdioProcessBestEffort(pid uint32) {
	_, _ = resumeStdioThreadsOnce(pid)
}
