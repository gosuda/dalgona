//go:build unix

package daemon

import (
	"errors"
	"fmt"
	"net"
	"os"

	"golang.org/x/sys/unix"
)

func peerUID(conn net.Conn) (int, error) {
	uc, ok := conn.(*net.UnixConn)
	if !ok {
		return -1, errors.New("daemon: peer requires unix connection")
	}
	raw, rawErr := uc.SyscallConn()
	if rawErr != nil {
		return -1, rawErr
	}
	var uid int
	var inner error
	if ctlErr := raw.Control(func(fd uintptr) {
		got, fdErr := peerUIDFromFD(int(fd))
		if fdErr != nil {
			inner = fdErr
			return
		}
		uid = got
	}); ctlErr != nil {
		return -1, ctlErr
	}
	if inner != nil {
		return -1, inner
	}
	return uid, nil
}

func verifyDialedPeer(conn net.Conn) error {
	got, err := peerUID(conn)
	if err != nil {
		return err
	}
	return checkUID(got, os.Getuid())
}

func verifyAcceptedPeer(conn net.Conn, daemonMarked bool) error {
	uc, ok := conn.(*net.UnixConn)
	if !ok {
		return errors.New("daemon: peer requires unix connection")
	}
	raw, rawErr := uc.SyscallConn()
	if rawErr != nil {
		return rawErr
	}
	var result error
	if ctlErr := raw.Control(func(fd uintptr) {
		uid, pid, credErr := peerCredsFromFD(int(fd))
		if credErr != nil {
			result = credErr
			return
		}
		if idErr := checkUID(uid, os.Getuid()); idErr != nil {
			result = idErr
			return
		}
		result = checkPeerSandbox(int(fd), pid, daemonMarked)
	}); ctlErr != nil {
		return ctlErr
	}
	return result
}

func verifyHome(home string) error {
	var stat unix.Stat_t
	if err := unix.Stat(home, &stat); err != nil {
		return fmt.Errorf("daemon home: %w", err)
	}
	if err := checkUID(int(stat.Uid), os.Getuid()); err != nil {
		return err
	}
	if perm := stat.Mode & 0o777; perm != 0o700 {
		return fmt.Errorf("daemon home mode %04o is not 0700", perm)
	}
	return nil
}

func verifySocket(path string) error {
	var stat unix.Stat_t
	if err := unix.Stat(path, &stat); err != nil {
		return fmt.Errorf("daemon socket: %w", err)
	}
	return checkUID(int(stat.Uid), os.Getuid())
}
