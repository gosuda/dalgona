//go:build gui && guiintegration

package desktop

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"
	"unsafe"

	"gioui.org/app"
	"golang.org/x/sys/windows"
)

const (
	testWMCommand     = 0x0111
	testIDOK          = 1
	testGWOwner       = 4
	testFileNameCombo = 1148
)

var (
	procTestFindWindowExW   = dialogUser32.NewProc("FindWindowExW")
	procTestGetWindow       = dialogUser32.NewProc("GetWindow")
	procTestSetDlgItemTextW = dialogUser32.NewProc("SetDlgItemTextW")
)

func ownedFileDialog(t *testing.T, h *dialogWindow) uintptr {
	t.Helper()
	owner := h.view.(app.Win32ViewEvent).HWND
	class, err := windows.UTF16PtrFromString("#32770")
	if err != nil {
		t.Fatal(err)
	}
	deadline := time.After(10 * time.Second)
	for {
		if dialog := findOwnedDialog(class, owner); dialog != 0 {
			return dialog
		}
		select {
		case <-deadline:
			t.Fatal("no visible file dialog owned by the window appeared")
		case <-time.After(20 * time.Millisecond):
		}
	}
}

func findOwnedDialog(class *uint16, owner uintptr) uintptr {
	var dialog uintptr
	for {
		dialog, _, _ = procTestFindWindowExW.Call(0, dialog, uintptr(unsafe.Pointer(class)), 0) //nolint:gosec // FindWindowExW reads the class name
		if dialog == 0 {
			return 0
		}
		if got, _, _ := procTestGetWindow.Call(dialog, testGWOwner); got == owner && windows.IsWindowVisible(windows.HWND(dialog)) {
			return dialog
		}
	}
}

func acceptOwnedDialog(t *testing.T, h *dialogWindow, prepare func(dialog uintptr)) DialogOutcome {
	t.Helper()
	dialog := ownedFileDialog(t, h)
	deadline := time.After(10 * time.Second)
	for {
		prepare(dialog)
		_, _, _ = procPostMessageW.Call(dialog, testWMCommand, testIDOK, 0)
		select {
		case got := <-h.desktop.DialogOutcomes():
			return got
		case <-deadline:
			t.Fatal("the file dialog never accepted its choice")
		case <-time.After(500 * time.Millisecond):
		}
	}
}

func TestSaveDialogReturnsAPathWithoutCreatingTheFile(t *testing.T) {
	h := openDialogWindow(t)
	req := ownedDialogRequests["save file"]
	req.SuggestedName = "dalgona-save-" + strconv.FormatInt(time.Now().UnixNano(), 36) + ".md"
	if err := h.desktop.ShowDialog(req); err != nil {
		t.Fatal(err)
	}
	got := acceptOwnedDialog(t, h, func(uintptr) {})
	if got.Err != nil || got.Result.Cancelled || len(got.Result.Files) != 1 || !filepath.IsAbs(got.Result.Files[0]) || filepath.Base(got.Result.Files[0]) != req.SuggestedName {
		t.Fatalf("save outcome %+v, want one absolute path named %s", got, req.SuggestedName)
	}
	if _, err := os.Lstat(got.Result.Files[0]); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("the save dialog created %s: %v", got.Result.Files[0], err)
	}
}

func TestFolderDialogReturnsAnExistingFolder(t *testing.T) {
	h := openDialogWindow(t)
	if err := h.desktop.ShowDialog(ownedDialogRequests["choose folder"]); err != nil {
		t.Fatal(err)
	}
	got := acceptOwnedDialog(t, h, func(uintptr) {})
	if got.Err != nil || len(got.Result.Files) != 1 {
		t.Fatalf("folder outcome %+v, want one folder", got)
	}
	if info, err := os.Stat(got.Result.Files[0]); err != nil || !info.IsDir() {
		t.Fatalf("the chosen folder %s is not a folder: %v", got.Result.Files[0], err)
	}
}

func TestOpenDialogReturnsTheChosenFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "chosen.png")
	if err := os.WriteFile(path, []byte("png"), 0o600); err != nil {
		t.Fatal(err)
	}
	text, wideErr := windows.UTF16PtrFromString(path)
	if wideErr != nil {
		t.Fatal(wideErr)
	}
	h := openDialogWindow(t)
	if err := h.desktop.ShowDialog(ownedDialogRequests["open files"]); err != nil {
		t.Fatal(err)
	}
	got := acceptOwnedDialog(t, h, func(dialog uintptr) {
		_, _, _ = procTestSetDlgItemTextW.Call(dialog, testFileNameCombo, uintptr(unsafe.Pointer(text))) //nolint:gosec // SetDlgItemTextW copies the path text
	})
	if got.Err != nil || len(got.Result.Files) != 1 {
		t.Fatalf("open outcome %+v, want %s", got, path)
	}
	want, statErr := os.Stat(path)
	if statErr != nil {
		t.Fatal(statErr)
	}
	if chosen, err := os.Stat(got.Result.Files[0]); err != nil || !os.SameFile(chosen, want) {
		t.Fatalf("the open dialog returned %s, want %s: %v", got.Result.Files[0], path, err)
	}
}
