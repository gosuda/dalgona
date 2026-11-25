//go:build !windows

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

	"github.com/creack/pty"
	"golang.org/x/sys/unix"
)

const shellVariable = "SHELL"

func interactiveShell(configured string) string {
	if configured == "" {
		return "/bin/sh"
	}
	return configured
}

type ptyTerminal struct {
	master *os.File
	cmd    *exec.Cmd
	done   chan struct{}
	exit   Exit
	close  sync.Once
}

func startTerminal(cmd *exec.Cmd, columns, rows int) (Terminal, error) {
	master, slave, err := openPTY(columns, rows)
	if err != nil {
		return nil, err
	}
	cmd.Stdin, cmd.Stdout, cmd.Stderr = slave, slave, slave
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true, Setctty: true}
	startErr := cmd.Start()
	closeErr := slave.Close()
	if startErr != nil {
		return nil, errors.Join(fmt.Errorf("runner: start terminal shell: %w", startErr), master.Close())
	}
	if closeErr != nil {
		return nil, errors.Join(fmt.Errorf("runner: release pty slave: %w", closeErr), cmd.Process.Kill(), master.Close())
	}
	t := &ptyTerminal{master: master, cmd: cmd, done: make(chan struct{})}
	go t.wait()
	return t, nil
}

func openPTY(columns, rows int) (*os.File, *os.File, error) {
	master, slave, err := pty.Open()
	if err != nil {
		return nil, nil, fmt.Errorf("runner: open pty: %w", err)
	}
	fd, dupErr := unix.FcntlInt(master.Fd(), unix.F_DUPFD_CLOEXEC, 0)
	if err := errors.Join(dupErr, master.Close()); err != nil {
		return nil, nil, errors.Join(fmt.Errorf("runner: detach pty master: %w", err), slave.Close())
	}
	if err := syscall.SetNonblock(fd, true); err != nil {
		return nil, nil, errors.Join(fmt.Errorf("runner: pty master: %w", err), syscall.Close(fd), slave.Close())
	}
	pollable := os.NewFile(uintptr(fd), master.Name())
	if err := setWindowSize(pollable, columns, rows); err != nil {
		return nil, nil, errors.Join(err, pollable.Close(), slave.Close())
	}
	return pollable, slave, nil
}

func setWindowSize(master *os.File, columns, rows int) error {
	if columns < 1 || rows < 1 {
		return errTerminalSize
	}
	conn, err := master.SyscallConn()
	if err != nil {
		return fmt.Errorf("runner: pty control: %w", err)
	}
	size := &unix.Winsize{Col: uint16(min(columns, 0xffff)), Row: uint16(min(rows, 0xffff))} //nolint:gosec // bounded to uint16 by min
	var ioctlErr error
	if err := conn.Control(func(fd uintptr) { ioctlErr = unix.IoctlSetWinsize(int(fd), unix.TIOCSWINSZ, size) }); err != nil { //nolint:gosec // a descriptor always fits in int
		return fmt.Errorf("runner: pty control: %w", err)
	}
	if ioctlErr != nil {
		return fmt.Errorf("runner: resize pty: %w", ioctlErr)
	}
	return nil
}

func (t *ptyTerminal) wait() {
	err := t.cmd.Wait()
	code, signal := exitDetails(t.cmd, err)
	t.exit = Exit{Code: code, Signal: signal, EndedAt: time.Now()}
	close(t.done)
}

func (t *ptyTerminal) Read(p []byte) (int, error) {
	n, err := t.master.Read(p)
	if errors.Is(err, syscall.EIO) || errors.Is(err, os.ErrClosed) {
		return n, io.EOF
	}
	return n, err
}

func (t *ptyTerminal) Write(p []byte) (int, error) { return t.master.Write(p) }

func (t *ptyTerminal) Resize(columns, rows int) error { return setWindowSize(t.master, columns, rows) }

func (t *ptyTerminal) Signal(signal Signal) error {
	target := syscall.SIGHUP
	if signal == SignalKill {
		target = syscall.SIGKILL
	}
	err := syscall.Kill(-t.cmd.Process.Pid, target)
	if errors.Is(err, syscall.ESRCH) {
		return nil
	}
	return err
}

func (t *ptyTerminal) Done() <-chan struct{} { return t.done }

func (t *ptyTerminal) Exit() Exit {
	<-t.done
	return t.exit
}

func (t *ptyTerminal) Close() error {
	var err error
	t.close.Do(func() { err = t.master.Close() })
	return err
}
