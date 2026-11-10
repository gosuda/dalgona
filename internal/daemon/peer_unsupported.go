//go:build unix && !linux && !darwin && !freebsd

package daemon

import "fmt"

func peerUIDFromFD(int) (int, error) {
	return -1, fmt.Errorf("%w: peer check unsupported", errUntrusted)
}
