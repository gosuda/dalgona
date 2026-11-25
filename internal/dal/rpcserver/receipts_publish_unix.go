//go:build !windows

package rpcserver

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

func prepareReceiptDirectory(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create receipt directory: %w", err)
	}
	return syncReceiptDirectory(filepath.Dir(dir))
}

func publishReceipt(temporary, path string) error {
	if err := os.Rename(temporary, path); err != nil {
		return err
	}
	return syncReceiptDirectory(filepath.Dir(path))
}

func syncReceiptDirectory(dir string) error {
	handle, err := os.Open(dir)
	if err != nil {
		return fmt.Errorf("open receipt directory: %w", err)
	}
	return errors.Join(handle.Sync(), handle.Close())
}
