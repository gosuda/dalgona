//go:build windows

package runner

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sync"
	"syscall"
	"time"
	"unsafe"

	"github.com/charmbracelet/x/conpty"
	"golang.org/x/sys/windows"
)

const shellVariable = "ComSpec"

func interactiveShell(configured string) string {
	if configured == "" {
		return `C:\Windows\System32\cmd.exe`
	}
	return configured
}

type conTerminal struct {
	console *conpty.ConPty
	output  *os.File
	process windows.Handle
	job     windows.Handle
	done    chan struct{}
	exit    Exit
	hangup  sync.Once
	close   sync.Once
}

func startTerminal(cmd *exec.Cmd, columns, rows int) (Terminal, error) {
	console, err := conpty.New(columns, rows, 0)
	if err != nil {
		return nil, fmt.Errorf("runner: open pseudo console: %w", err)
	}
	output, err := duplicateOutput(windows.Handle(console.OutPipeReadFd()))
	if err != nil {
		return nil, errors.Join(err, console.Close())
	}
	job, err := newTerminalJob()
	if err != nil {
		return nil, errors.Join(err, output.Close(), console.Close())
	}
	process, err := spawnSuspended(console, cmd, job)
	if err != nil {
		return nil, errors.Join(err, windows.CloseHandle(job), output.Close(), console.Close())
	}
	t := &conTerminal{console: console, output: output, process: process, job: job, done: make(chan struct{})}
	go t.wait()
	return t, nil
}

func spawnSuspended(console *conpty.ConPty, cmd *exec.Cmd, job windows.Handle) (windows.Handle, error) {
	sys := cmd.SysProcAttr
	if sys == nil {
		sys = &syscall.SysProcAttr{}
	}
	sys.CreationFlags |= windows.CREATE_SUSPENDED
	pid, handle, err := console.Spawn(cmd.Path, cmd.Args, &syscall.ProcAttr{Dir: cmd.Dir, Env: cmd.Env, Sys: sys})
	if err != nil {
		return 0, fmt.Errorf("runner: start terminal shell: %w", err)
	}
	process := windows.Handle(handle)
	id, ok := windowsPID(pid)
	if !ok {
		return 0, errors.Join(fmt.Errorf("runner: invalid terminal pid %d", pid), windows.TerminateProcess(process, 1), windows.CloseHandle(process))
	}
	if err := windows.AssignProcessToJobObject(job, process); err != nil {
		return 0, errors.Join(fmt.Errorf("runner: assign terminal job: %w", err), windows.TerminateProcess(process, 1), windows.CloseHandle(process))
	}
	if err := resumeCommandProcess(id); err != nil {
		return 0, errors.Join(err, windows.TerminateJobObject(job, 1), windows.CloseHandle(process))
	}
	return process, nil
}

func duplicateOutput(source windows.Handle) (*os.File, error) {
	current := windows.CurrentProcess()
	var duplicate windows.Handle
	if err := windows.DuplicateHandle(current, source, current, &duplicate, 0, false, windows.DUPLICATE_SAME_ACCESS); err != nil {
		return nil, fmt.Errorf("runner: duplicate pseudo console output: %w", err)
	}
	return os.NewFile(uintptr(duplicate), "conpty-output"), nil
}

func newTerminalJob() (windows.Handle, error) {
	job, err := windows.CreateJobObject(nil, nil)
	if err != nil {
		return 0, fmt.Errorf("runner: create terminal job: %w", err)
	}
	var limits windows.JOBOBJECT_EXTENDED_LIMIT_INFORMATION
	limits.BasicLimitInformation.LimitFlags = windows.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
	if _, err := windows.SetInformationJobObject(job, windows.JobObjectExtendedLimitInformation, uintptr(unsafe.Pointer(&limits)), uint32(unsafe.Sizeof(limits))); err != nil { //nolint:gosec // SetInformationJobObject takes the limits struct as a raw pointer
		return 0, errors.Join(fmt.Errorf("runner: set terminal job kill-on-close: %w", err), windows.CloseHandle(job))
	}
	return job, nil
}

func (t *conTerminal) wait() {
	_, waitErr := windows.WaitForSingleObject(t.process, windows.INFINITE)
	var code uint32
	codeErr := windows.GetExitCodeProcess(t.process, &code)
	t.exit = Exit{Code: int(code), EndedAt: time.Now()}
	if waitErr != nil || codeErr != nil {
		t.exit.Code = 1
	}
	_ = windows.CloseHandle(t.process)
	close(t.done)
	t.hangupConsole()
}

func (t *conTerminal) hangupConsole() {
	t.hangup.Do(func() { _ = t.console.Close() })
}

func (t *conTerminal) Read(p []byte) (int, error) {
	n, err := t.output.Read(p)
	if errors.Is(err, windows.ERROR_BROKEN_PIPE) || errors.Is(err, os.ErrClosed) {
		return n, io.EOF
	}
	return n, err
}

func (t *conTerminal) Write(p []byte) (int, error) { return t.console.Write(p) }

func (t *conTerminal) Resize(columns, rows int) error {
	if columns < 1 || rows < 1 {
		return errTerminalSize
	}
	return t.console.Resize(columns, rows)
}

func (t *conTerminal) Signal(signal Signal) error {
	if signal == SignalKill {
		return windows.TerminateJobObject(t.job, jobExitCode(signal))
	}
	t.hangupConsole()
	return nil
}

func (t *conTerminal) Done() <-chan struct{} { return t.done }

func (t *conTerminal) Exit() Exit {
	<-t.done
	return t.exit
}

func (t *conTerminal) Close() error {
	var err error
	t.close.Do(func() {
		t.hangupConsole()
		err = errors.Join(t.output.Close(), windows.CloseHandle(t.job))
	})
	return err
}
