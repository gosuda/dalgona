package daemon

import (
	"errors"
	"fmt"
)

var errUntrusted = errors.New("daemon: untrusted socket")

func checkUID(got, want int) error {
	if got != want {
		return fmt.Errorf("%w: uid %d is not %d", errUntrusted, got, want)
	}
	return nil
}
