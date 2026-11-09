//go:build !linux

package runner

import "os"

func selfExe() (string, error) {
	return os.Executable()
}
