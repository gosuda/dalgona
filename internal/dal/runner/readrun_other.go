//go:build !windows

package runner

import (
	"os"
)

func readRunFile(path string) ([]byte, error) {
	return os.ReadFile(path)
}
