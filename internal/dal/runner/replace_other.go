//go:build !windows

package runner

import (
	"io/fs"
	"os"
)

func publishReplace(tmp, dst string, _ fs.FileMode) error {
	return os.Rename(tmp, dst) //nolint:gosec // paths are managed by the runner caller
}
