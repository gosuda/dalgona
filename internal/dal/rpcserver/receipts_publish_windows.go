package rpcserver

import (
	"fmt"
	"os"

	"golang.org/x/sys/windows"
)

func prepareReceiptDirectory(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create receipt directory: %w", err)
	}
	return nil
}

func publishReceipt(temporary, path string) error {
	source, err := windows.UTF16PtrFromString(temporary)
	if err != nil {
		return fmt.Errorf("encode temporary receipt path: %w", err)
	}
	destination, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return fmt.Errorf("encode receipt destination path: %w", err)
	}
	if err := windows.MoveFileEx(source, destination, windows.MOVEFILE_REPLACE_EXISTING|windows.MOVEFILE_WRITE_THROUGH); err != nil {
		return fmt.Errorf("move receipt with write-through: %w", err)
	}
	return nil
}
