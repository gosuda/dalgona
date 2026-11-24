//go:build gui && windows

package desktop

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"runtime"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const (
	comQueryInterface         = 0
	comRelease                = 2
	modalWindowShow           = 3
	fileDialogSetFileTypes    = 4
	fileDialogSetOptions      = 9
	fileDialogGetOptions      = 10
	fileDialogSetFileName     = 15
	fileDialogSetTitle        = 17
	fileDialogGetResult       = 20
	fileDialogSetDefaultExt   = 22
	fileOpenDialogGetResults  = 27
	oleWindowGetWindow        = 3
	shellItemGetDisplayName   = 5
	shellItemArrayGetCount    = 7
	shellItemArrayGetItemAt   = 8
	clsctxInprocServer        = 0x1
	sigdnFileSysPath          = 0x80058000
	dwordBits                 = 0xFFFFFFFF
	hresultCancelled          = 0x800704C7
	wmClose                   = 0x0010
	wmTimer                   = 0x0113
	wmDialogWake              = 0x8001
	dialogCloseTimer          = 1
	dialogCloseRetryMillis    = 100
	widePathLimit             = 32767
	hwndMessage               = ^uintptr(2)
	coinitApartmentNoOLE1DDE  = windows.COINIT_APARTMENTTHREADED | windows.COINIT_DISABLE_OLE1DDE
	fileDialogWakeClassName   = "DalgonaFileDialogWake"
	fileDialogRegisterFailure = "register the file dialog wake window: %w; restart dalgona"
)

var (
	clsidFileOpenDialog = windows.GUID{Data1: 0xdc1c5a9c, Data2: 0xe88a, Data3: 0x4dde, Data4: [8]byte{0xa5, 0xa1, 0x60, 0xf8, 0x2a, 0x20, 0xae, 0xf7}}
	iidFileOpenDialog   = windows.GUID{Data1: 0xd57c7288, Data2: 0xd4ad, Data3: 0x4768, Data4: [8]byte{0xbe, 0x02, 0x9d, 0x96, 0x95, 0x32, 0xd9, 0x60}}
	clsidFileSaveDialog = windows.GUID{Data1: 0xc0b4e2f3, Data2: 0xba21, Data3: 0x4773, Data4: [8]byte{0x8d, 0xba, 0x33, 0x5e, 0xc9, 0x46, 0xeb, 0x8b}}
	iidFileSaveDialog   = windows.GUID{Data1: 0x84bccd23, Data2: 0x5fde, Data3: 0x4cdb, Data4: [8]byte{0xae, 0xa4, 0xaf, 0x64, 0xb8, 0x3d, 0x78, 0xab}}
	iidOleWindow        = windows.GUID{Data1: 0x00000114, Data4: [8]byte{0xc0, 0, 0, 0, 0, 0, 0, 0x46}}

	dialogOle32          = windows.NewLazySystemDLL("ole32.dll")
	dialogUser32         = windows.NewLazySystemDLL("user32.dll")
	procCoCreateInstance = dialogOle32.NewProc("CoCreateInstance")
	procRegisterClassExW = dialogUser32.NewProc("RegisterClassExW")
	procCreateWindowExW  = dialogUser32.NewProc("CreateWindowExW")
	procDestroyWindow    = dialogUser32.NewProc("DestroyWindow")
	procDefWindowProcW   = dialogUser32.NewProc("DefWindowProcW")
	procPostMessageW     = dialogUser32.NewProc("PostMessageW")
	procSetTimer         = dialogUser32.NewProc("SetTimer")

	errEmptySelection = errors.New("the file dialog closed without a selection; choose a file, then try again")
	dialogWakers      sync.Map
	dialogWakeClass   = sync.OnceValues(registerDialogWakeClass)
)

type comObject struct {
	vtbl *[32]uintptr
}

type filterSpec struct {
	name    *uint16
	pattern *uint16
}

type windowClass struct {
	size       uint32
	style      uint32
	procedure  uintptr
	classExtra int32
	extra      int32
	instance   windows.Handle
	icon       uintptr
	cursor     uintptr
	background uintptr
	menu       *uint16
	name       *uint16
	smallIcon  uintptr
}

type wakeClass struct {
	name     *uint16
	instance windows.Handle
}

type fileDialog struct {
	com    *comObject
	kind   platform.DialogKind
	pinner runtime.Pinner
}

type dialogAnswer struct {
	result platform.DialogResult
	err    error
}

func showFileDialog(ctx context.Context, owner uintptr, req platform.DialogRequest) (platform.DialogResult, error) {
	answers := make(chan dialogAnswer, 1)
	go func() {
		runtime.LockOSThread()
		result, err := runFileDialog(ctx, owner, req)
		answers <- dialogAnswer{result, err}
	}()
	answer := <-answers
	return answer.result, answer.err
}

func runFileDialog(ctx context.Context, owner uintptr, req platform.DialogRequest) (platform.DialogResult, error) {
	if err := windows.CoInitializeEx(0, coinitApartmentNoOLE1DDE); err != nil && !errors.Is(err, syscall.Errno(windows.S_FALSE)) {
		return platform.DialogResult{}, fmt.Errorf("start COM for the file dialog: %w; restart dalgona", err)
	}
	defer windows.CoUninitialize()
	dialog, err := newFileDialog(req)
	if err != nil {
		return platform.DialogResult{}, err
	}
	defer dialog.release()
	hr, err := dialog.show(ctx, owner)
	switch {
	case err != nil:
		return platform.DialogResult{}, err
	case hr&dwordBits == hresultCancelled:
		return platform.DialogResult{Cancelled: true}, nil
	case failed(hr):
		return platform.DialogResult{}, hresultError("show the file dialog; close other dialogs, then try again", hr)
	}
	files, err := dialog.result()
	if err != nil {
		return platform.DialogResult{}, err
	}
	return platform.DialogResult{Files: files}, nil
}

func newFileDialog(req platform.DialogRequest) (*fileDialog, error) {
	class, iid := &clsidFileOpenDialog, &iidFileOpenDialog
	if req.Kind == platform.DialogSaveFile {
		class, iid = &clsidFileSaveDialog, &iidFileSaveDialog
	}
	d := &fileDialog{kind: req.Kind}
	if hr, _, _ := procCoCreateInstance.Call(uintptr(unsafe.Pointer(class)), 0, clsctxInprocServer, uintptr(unsafe.Pointer(iid)), uintptr(unsafe.Pointer(&d.com))); failed(hr) { //nolint:gosec // CoCreateInstance reads the class and interface IDs and writes the dialog pointer
		return nil, hresultError("create the Windows file dialog; update Windows", hr)
	}
	if err := d.configure(req); err != nil {
		d.release()
		return nil, err
	}
	return d, nil
}

func (d *fileDialog) configure(req platform.DialogRequest) error {
	var options uint32
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[fileDialogGetOptions], d.com.this(), uintptr(unsafe.Pointer(&options))); failed(hr) { //nolint:gosec // GetOptions writes one FILEOPENDIALOGOPTIONS value
		return hresultError("read the file dialog options", hr)
	}
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[fileDialogSetOptions], d.com.this(), uintptr(options)|fileDialogOptions(req)); failed(hr) {
		return hresultError("set the file dialog options", hr)
	}
	if err := d.setText(fileDialogSetTitle, req.Title); err != nil {
		return err
	}
	if err := d.setFileTypes(req.Filters); err != nil {
		return err
	}
	if req.Kind != platform.DialogSaveFile {
		return nil
	}
	if err := d.setText(fileDialogSetFileName, req.SuggestedName); err != nil {
		return err
	}
	if len(req.Filters) == 0 {
		return nil
	}
	return d.setText(fileDialogSetDefaultExt, req.Filters[0].Extensions[0])
}

func (d *fileDialog) setText(slot int, text string) error {
	wide, err := d.wide(text)
	if err != nil {
		return err
	}
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[slot], d.com.this(), uintptr(unsafe.Pointer(wide))); failed(hr) { //nolint:gosec // the dialog reads the pinned NUL-terminated text
		return hresultError("prepare the file dialog text", hr)
	}
	return nil
}

func (d *fileDialog) setFileTypes(filters []platform.FileFilter) error {
	if len(filters) == 0 {
		return nil
	}
	specs := make([]filterSpec, len(filters))
	for i, f := range filters {
		name, err := d.wide(f.Name)
		if err != nil {
			return err
		}
		pattern, err := d.wide(filterPattern(f.Extensions))
		if err != nil {
			return err
		}
		specs[i] = filterSpec{name: name, pattern: pattern}
	}
	d.pinner.Pin(&specs[0])
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[fileDialogSetFileTypes], d.com.this(), uintptr(len(specs)), uintptr(unsafe.Pointer(&specs[0]))); failed(hr) { //nolint:gosec // SetFileTypes reads len(specs) pinned COMDLG_FILTERSPEC entries
		return hresultError("set the file dialog filters", hr)
	}
	return nil
}

func (d *fileDialog) wide(text string) (*uint16, error) {
	wide, err := windows.UTF16PtrFromString(text)
	if err != nil {
		return nil, fmt.Errorf("the file dialog text %q contains NUL; remove it, then try again", text)
	}
	d.pinner.Pin(wide)
	return wide, nil
}

func (d *fileDialog) show(ctx context.Context, owner uintptr) (uintptr, error) {
	waker, err := newDialogWaker(d.com)
	if err != nil {
		return 0, err
	}
	woken := make(chan struct{})
	stop := context.AfterFunc(ctx, func() {
		defer close(woken)
		_, _, _ = procPostMessageW.Call(waker, wmDialogWake, 0, 0)
	})
	var hr uintptr
	if ctx.Err() == nil {
		hr, _, _ = syscall.SyscallN(d.com.vtbl[modalWindowShow], d.com.this(), owner)
	}
	if !stop() {
		<-woken
	}
	closeDialogWaker(waker)
	return hr, ctx.Err()
}

func (d *fileDialog) result() ([]string, error) {
	if d.kind == platform.DialogOpenFiles {
		return d.selection()
	}
	var item *comObject
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[fileDialogGetResult], d.com.this(), uintptr(unsafe.Pointer(&item))); failed(hr) || item == nil { //nolint:gosec // GetResult writes one IShellItem pointer
		return nil, errEmptySelection
	}
	defer item.release()
	path, err := itemPath(item)
	if err != nil {
		return nil, err
	}
	return []string{path}, nil
}

func (d *fileDialog) selection() ([]string, error) {
	var items *comObject
	if hr, _, _ := syscall.SyscallN(d.com.vtbl[fileOpenDialogGetResults], d.com.this(), uintptr(unsafe.Pointer(&items))); failed(hr) || items == nil { //nolint:gosec // GetResults writes one IShellItemArray pointer
		return nil, errEmptySelection
	}
	defer items.release()
	var count uint32
	if hr, _, _ := syscall.SyscallN(items.vtbl[shellItemArrayGetCount], items.this(), uintptr(unsafe.Pointer(&count))); failed(hr) || count == 0 { //nolint:gosec // GetCount writes one DWORD
		return nil, errEmptySelection
	}
	var paths []string
	for i := range count {
		path, err := itemAt(items, i)
		if err != nil {
			return nil, err
		}
		paths = append(paths, path)
	}
	return paths, nil
}

func (d *fileDialog) release() {
	d.pinner.Unpin()
	d.com.release()
}

func itemAt(items *comObject, index uint32) (string, error) {
	var item *comObject
	if hr, _, _ := syscall.SyscallN(items.vtbl[shellItemArrayGetItemAt], items.this(), uintptr(index), uintptr(unsafe.Pointer(&item))); failed(hr) || item == nil { //nolint:gosec // GetItemAt writes one IShellItem pointer
		return "", hresultError("read the selected file", hr)
	}
	defer item.release()
	return itemPath(item)
}

func itemPath(item *comObject) (string, error) {
	var wide *uint16
	if hr, _, _ := syscall.SyscallN(item.vtbl[shellItemGetDisplayName], item.this(), sigdnFileSysPath, uintptr(unsafe.Pointer(&wide))); failed(hr) || wide == nil { //nolint:gosec // GetDisplayName writes one CoTaskMem string pointer
		return "", platform.ErrNotLocalFile
	}
	defer windows.CoTaskMemFree(unsafe.Pointer(wide)) //nolint:gosec // the shell allocated the path with CoTaskMemAlloc
	return fileSystemPath(wide)
}

func fileSystemPath(wide *uint16) (string, error) {
	n := 0
	for n <= widePathLimit && *(*uint16)(unsafe.Add(unsafe.Pointer(wide), 2*n)) != 0 { //nolint:gosec // the scan stops at the first NUL or one unit past the longest Windows path
		n++
	}
	if n == 0 || n > widePathLimit {
		return "", platform.ErrNotLocalFile
	}
	path := windows.UTF16ToString(unsafe.Slice(wide, n)) //nolint:gosec // the scan found n units before the NUL
	if !filepath.IsAbs(path) {
		return "", platform.ErrNotLocalFile
	}
	return path, nil
}

func newDialogWaker(dialog *comObject) (uintptr, error) {
	class, err := dialogWakeClass()
	if err != nil {
		return 0, err
	}
	hwnd, _, callErr := procCreateWindowExW.Call(0, uintptr(unsafe.Pointer(class.name)), 0, 0, 0, 0, 0, 0, hwndMessage, 0, uintptr(class.instance), 0) //nolint:gosec // CreateWindowExW reads the registered class name
	if hwnd == 0 {
		return 0, fmt.Errorf("create the file dialog wake window: %w; close unused windows, then try again", callErr)
	}
	dialogWakers.Store(hwnd, dialog)
	return hwnd, nil
}

func closeDialogWaker(hwnd uintptr) {
	dialogWakers.Delete(hwnd)
	_, _, _ = procDestroyWindow.Call(hwnd)
}

func registerDialogWakeClass() (wakeClass, error) {
	name, err := windows.UTF16PtrFromString(fileDialogWakeClassName)
	if err != nil {
		return wakeClass{}, fmt.Errorf(fileDialogRegisterFailure, err)
	}
	var instance windows.Handle
	if err := windows.GetModuleHandleEx(0, nil, &instance); err != nil {
		return wakeClass{}, fmt.Errorf(fileDialogRegisterFailure, err)
	}
	class := windowClass{procedure: syscall.NewCallback(dialogWakeProc), instance: instance, name: name}
	class.size = uint32(unsafe.Sizeof(class))
	if atom, _, callErr := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&class))); atom == 0 { //nolint:gosec // RegisterClassExW copies the WNDCLASSEXW
		return wakeClass{}, fmt.Errorf(fileDialogRegisterFailure, callErr)
	}
	return wakeClass{name: name, instance: instance}, nil
}

func dialogWakeProc(hwnd, message, wParam, lParam uintptr) uintptr {
	if message&dwordBits != wmDialogWake && message&dwordBits != wmTimer {
		result, _, _ := procDefWindowProcW.Call(hwnd, message, wParam, lParam)
		return result
	}
	found, _ := dialogWakers.Load(hwnd)
	if dialog, _ := found.(*comObject); dialog != nil {
		closeDialogWindow(dialog)
	}
	_, _, _ = procSetTimer.Call(hwnd, dialogCloseTimer, dialogCloseRetryMillis, 0)
	return 0
}

func closeDialogWindow(dialog *comObject) {
	var window *comObject
	if hr, _, _ := syscall.SyscallN(dialog.vtbl[comQueryInterface], dialog.this(), uintptr(unsafe.Pointer(&iidOleWindow)), uintptr(unsafe.Pointer(&window))); failed(hr) || window == nil { //nolint:gosec // QueryInterface reads the IID and writes the IOleWindow pointer
		return
	}
	defer window.release()
	var hwnd uintptr
	if hr, _, _ := syscall.SyscallN(window.vtbl[oleWindowGetWindow], window.this(), uintptr(unsafe.Pointer(&hwnd))); failed(hr) || hwnd == 0 { //nolint:gosec // GetWindow writes one HWND
		return
	}
	_, _, _ = procPostMessageW.Call(hwnd, wmClose, 0, 0)
}

func (o *comObject) this() uintptr {
	return uintptr(unsafe.Pointer(o)) //nolint:gosec // the interface pointer is the COM this argument
}

func (o *comObject) release() {
	if o != nil {
		_, _, _ = syscall.SyscallN(o.vtbl[comRelease], o.this())
	}
}

func failed(hr uintptr) bool {
	return hr&0x80000000 != 0
}

func hresultError(action string, hr uintptr) error {
	return fmt.Errorf("%s (HRESULT 0x%08X)", action, hr&dwordBits)
}
