//go:build windows

package runner

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	pinnedAccess = windows.FILE_GENERIC_READ
	pinnedShare  = windows.FILE_SHARE_READ | windows.FILE_SHARE_WRITE | windows.FILE_SHARE_DELETE
)

func openPinned(root, rel string) (*os.File, error) {
	current, err := openFollowedDirectory(root)
	if err != nil {
		return nil, fmt.Errorf("runner: pinned open %s: %w", rel, err)
	}
	if rel == "." {
		return os.NewFile(uintptr(current), root), nil
	}
	parts := strings.Split(filepath.ToSlash(rel), "/")
	for index, part := range parts {
		next, openErr := openComponent(current, part, index < len(parts)-1)
		_ = windows.CloseHandle(current)
		if openErr != nil {
			return nil, pinnedOpenError(rel, openErr)
		}
		current = next
	}
	return os.NewFile(uintptr(current), filepath.Join(root, rel)), nil
}

func openNoFollow(path string) (*os.File, error) {
	parent, err := openFollowedDirectory(filepath.Dir(path))
	if err != nil {
		return nil, &fs.PathError{Op: "open", Path: path, Err: err}
	}
	handle, err := openComponent(parent, filepath.Base(path), false)
	_ = windows.CloseHandle(parent)
	if err != nil {
		return nil, pinnedOpenError(path, err)
	}
	return os.NewFile(uintptr(handle), path), nil
}

func openFollowedDirectory(path string) (windows.Handle, error) {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return 0, err
	}
	return windows.CreateFile(name, pinnedAccess, pinnedShare, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS, 0)
}

func openComponent(parent windows.Handle, name string, directory bool) (windows.Handle, error) {
	objectName, err := windows.NewNTUnicodeString(name)
	if err != nil {
		return 0, err
	}
	attributes := windows.OBJECT_ATTRIBUTES{
		RootDirectory: parent,
		ObjectName:    objectName,
		Attributes:    windows.OBJ_CASE_INSENSITIVE | windows.OBJ_DONT_REPARSE,
	}
	attributes.Length = uint32(unsafe.Sizeof(attributes))
	options := uint32(windows.FILE_SYNCHRONOUS_IO_NONALERT | windows.FILE_OPEN_FOR_BACKUP_INTENT)
	if directory {
		options |= windows.FILE_DIRECTORY_FILE
	}
	handle, err := createComponent(&attributes, options)
	if errors.Is(err, windows.STATUS_INVALID_PARAMETER) {
		attributes.Attributes &^= windows.OBJ_DONT_REPARSE
		return openReparsePointRefused(&attributes, options)
	}
	return handle, err
}

func openReparsePointRefused(attributes *windows.OBJECT_ATTRIBUTES, options uint32) (windows.Handle, error) {
	handle, err := createComponent(attributes, options|windows.FILE_OPEN_REPARSE_POINT)
	if err != nil {
		return 0, err
	}
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(handle, &info); err != nil {
		_ = windows.CloseHandle(handle)
		return 0, err
	}
	if info.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
		_ = windows.CloseHandle(handle)
		return 0, windows.STATUS_REPARSE_POINT_ENCOUNTERED
	}
	return handle, nil
}

func createComponent(attributes *windows.OBJECT_ATTRIBUTES, options uint32) (windows.Handle, error) {
	var handle windows.Handle
	err := windows.NtCreateFile(&handle, pinnedAccess, attributes, &windows.IO_STATUS_BLOCK{}, nil, 0, pinnedShare, windows.FILE_OPEN, options, 0, 0)
	return handle, err
}

func pinnedOpenError(path string, err error) error {
	if errors.Is(err, windows.STATUS_REPARSE_POINT_ENCOUNTERED) {
		return fmt.Errorf("%w: %s", ErrPinnedSymlink, path)
	}
	if status, ok := errors.AsType[windows.NTStatus](err); ok {
		err = status.Errno()
	}
	return &fs.PathError{Op: "open", Path: path, Err: err}
}
