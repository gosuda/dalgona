//go:build !windows && !linux && !darwin

package runner

import "errors"

func waitProcessExit(int, string) error {
	return errors.New("runner: waiting for a shim exit needs pidfd or kqueue")
}
