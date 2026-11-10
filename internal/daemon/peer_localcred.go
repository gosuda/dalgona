//go:build darwin || freebsd

package daemon

import "golang.org/x/sys/unix"

func peerUIDFromFD(fd int) (int, error) {
	xucred, err := unix.GetsockoptXucred(fd, unix.SOL_LOCAL, unix.LOCAL_PEERCRED)
	if err != nil {
		return -1, err
	}
	return int(xucred.Uid), nil
}
