//go:build gui

package desktop

import (
	"strings"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const (
	fosOverwritePrompt  = 0x2
	fosStrictFileTypes  = 0x4
	fosNoChangeDir      = 0x8
	fosPickFolders      = 0x20
	fosForceFileSystem  = 0x40
	fosAllowMultiSelect = 0x200
	fosPathMustExist    = 0x800
	fosFileMustExist    = 0x1000
	fosNoReadOnlyReturn = 0x8000
	fosNoTestFileCreate = 0x10000
)

func fileDialogOptions(req platform.DialogRequest) uintptr {
	options := uintptr(fosNoChangeDir | fosForceFileSystem | fosPathMustExist)
	switch req.Kind {
	case platform.DialogOpenFiles:
		options |= fosAllowMultiSelect | fosFileMustExist
	case platform.DialogChooseFolder:
		options |= fosPickFolders
	case platform.DialogSaveFile:
		options |= fosOverwritePrompt | fosNoReadOnlyReturn | fosNoTestFileCreate
		if len(req.Filters) > 0 {
			options |= fosStrictFileTypes
		}
	}
	return options
}

func filterPattern(extensions []string) string {
	return "*." + strings.Join(extensions, ";*.")
}

func splitPaths(encoded []byte) ([]string, error) {
	paths := strings.Split(string(encoded), "\x00")
	for _, path := range paths {
		if !strings.HasPrefix(path, "/") {
			return nil, platform.ErrNotLocalFile
		}
	}
	return paths, nil
}
