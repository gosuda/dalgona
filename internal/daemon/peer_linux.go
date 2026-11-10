//go:build linux

package daemon

import "golang.org/x/sys/unix"

func peerUIDFromFD(fd int) (int, error) {
	ucred, err := unix.GetsockoptUcred(fd, unix.SOL_SOCKET, unix.SO_PEERCRED)
	if err != nil {
		return -1, err
	}
	return int(ucred.Uid), nil
}
