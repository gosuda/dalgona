package daemon

import "fmt"

func checkSocketPathLimit(home string) error {
	socket := SocketPath(home)
	if len(socket) >= socketPathLimit {
		return fmt.Errorf("daemon socket path %s is %d bytes; the limit is %d; use a shorter home directory", socket, len(socket), socketPathLimit)
	}
	return nil
}
