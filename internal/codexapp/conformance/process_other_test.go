//go:build !linux

package conformance

import (
	"errors"
	"time"
)

const processTableAvailable = false

var errNoProcessTable = errors.New("daemon cleanup needs the Linux /proc process table")

func daemonPIDs(string, string) ([]int, error) { return nil, errNoProcessTable }

func terminate(int) error { return errNoProcessTable }

func kill(int) error { return errNoProcessTable }

func waitPID(int, time.Time) bool { return false }

func lockFile(string, time.Duration) (func() error, error) { return nil, errNoProcessTable }
