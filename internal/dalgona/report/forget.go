package report

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

func ForgetAppToken(home string) error {
	dir := filepath.Join(home, reportDirName)
	err := os.Remove(filepath.Join(dir, credentialFileName))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("failure reporting remove authorization: %w", err)
	}
	syncReporterParent(dir)
	return nil
}
