//go:build !unix

package runner

import "errors"

func makeFifo(string) error {
	return errors.New("runner: named pipes are unavailable on this platform")
}
