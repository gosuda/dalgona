//go:build windows

package platform

import (
	"context"
	"fmt"
	"runtime"
	"sync"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

const (
	spiGetHighContrast         = 0x0042
	spiSetHighContrast         = 0x0043
	spiGetClientAreaAnimation  = 0x1042
	spiSetClientAreaAnimation  = 0x1043
	hcfHighContrastOn          = 0x00000001
	personalizeKey             = `Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`
	appsUseLightTheme          = "AppsUseLightTheme"
	appearanceWindowClass      = "DalgonaAppearanceWatcher"
	wmDestroy                  = 0x0002
	wmClose                    = 0x0010
	wmSysColorChange           = 0x0015
	wmSettingChange            = 0x001A
	wmThemeChanged             = 0x031A
	settingChangeImmersiveArea = 0
)

var (
	preferencesUser32        = windows.NewLazySystemDLL("user32.dll")
	procSystemParametersInfo = preferencesUser32.NewProc("SystemParametersInfoW")
	procRegisterClassExW     = preferencesUser32.NewProc("RegisterClassExW")
	procCreateWindowExW      = preferencesUser32.NewProc("CreateWindowExW")
	procDestroyWindow        = preferencesUser32.NewProc("DestroyWindow")
	procDefWindowProcW       = preferencesUser32.NewProc("DefWindowProcW")
	procGetMessageW          = preferencesUser32.NewProc("GetMessageW")
	procDispatchMessageW     = preferencesUser32.NewProc("DispatchMessageW")
	procPostMessageW         = preferencesUser32.NewProc("PostMessageW")
	procPostQuitMessage      = preferencesUser32.NewProc("PostQuitMessage")

	appearanceWatchers sync.Map
	appearanceClass    = sync.OnceValues(registerAppearanceClass)
)

type highContrast struct {
	size          uint32
	flags         uint32
	defaultScheme *uint16
}

type windowClass struct {
	size       uint32
	style      uint32
	wndProc    uintptr
	clsExtra   int32
	wndExtra   int32
	instance   windows.Handle
	icon       windows.Handle
	cursor     windows.Handle
	background windows.Handle
	menuName   *uint16
	className  *uint16
	iconSmall  windows.Handle
}

type windowMessage struct {
	hwnd    uintptr
	message uint32
	wParam  uintptr
	lParam  uintptr
	time    uint32
	pointX  int32
	pointY  int32
	private uint32
}

type registeredClass struct {
	name     *uint16
	instance windows.Handle
}

func ReducedMotion(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	var animate int32
	if err := systemParameter(spiGetClientAreaAnimation, unsafe.Pointer(&animate)); err != nil { //nolint:gosec // SPI_GETCLIENTAREAANIMATION writes one Win32 BOOL
		return false, fmt.Errorf("read the Windows animation setting: %w", err)
	}
	return animate == 0, nil
}

func HighContrast(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	contrast := highContrast{size: uint32(unsafe.Sizeof(highContrast{}))}
	if err := systemParameter(spiGetHighContrast, unsafe.Pointer(&contrast)); err != nil { //nolint:gosec // SPI_GETHIGHCONTRAST writes the sized HIGHCONTRASTW struct
		return false, fmt.Errorf("read the Windows high contrast setting: %w", err)
	}
	return contrast.flags&hcfHighContrastOn != 0, nil
}

func PrefersDark(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	k, err := registry.OpenKey(registry.CURRENT_USER, personalizeKey, registry.QUERY_VALUE)
	if err != nil {
		return false, fmt.Errorf("read the Windows app colour mode: %w", err)
	}
	defer k.Close()
	light, _, err := k.GetIntegerValue(appsUseLightTheme)
	if err != nil {
		return false, fmt.Errorf("read the Windows app colour mode: %w", err)
	}
	return light == 0, nil
}

func WatchAppearance(ctx context.Context, changed func()) error {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	class, err := appearanceClass()
	if err != nil {
		return err
	}
	hwnd, _, err := procCreateWindowExW.Call(0, uintptr(unsafe.Pointer(class.name)), 0, 0, 0, 0, 0, 0, 0, 0, uintptr(class.instance), 0) //nolint:gosec // CreateWindowExW reads the live, NUL-terminated class name
	if hwnd == 0 {
		return fmt.Errorf("create the appearance watcher window: %w", err)
	}
	appearanceWatchers.Store(hwnd, changed)
	defer appearanceWatchers.Delete(hwnd)
	stop := context.AfterFunc(ctx, func() { _, _, _ = procPostMessageW.Call(hwnd, wmClose, 0, 0) })
	defer stop()
	if err := pumpMessages(); err != nil {
		_, _, _ = procDestroyWindow.Call(hwnd)
		return err
	}
	return nil
}

func registerAppearanceClass() (registeredClass, error) {
	name, err := windows.UTF16PtrFromString(appearanceWindowClass)
	if err != nil {
		return registeredClass{}, err
	}
	var instance windows.Handle
	if err := windows.GetModuleHandleEx(0, nil, &instance); err != nil {
		return registeredClass{}, fmt.Errorf("find the dalgona module for the appearance watcher: %w", err)
	}
	class := windowClass{size: uint32(unsafe.Sizeof(windowClass{})), wndProc: windows.NewCallback(appearanceWindowProc), instance: instance, className: name}
	if atom, _, err := procRegisterClassExW.Call(uintptr(unsafe.Pointer(&class))); atom == 0 { //nolint:gosec // RegisterClassExW reads the sized WNDCLASSEXW during this call
		return registeredClass{}, fmt.Errorf("register the appearance watcher window class: %w", err)
	}
	return registeredClass{name: name, instance: instance}, nil
}

func appearanceWindowProc(hwnd, message, wParam, lParam uintptr) uintptr {
	switch message {
	case wmSettingChange, wmSysColorChange, wmThemeChanged:
		notifyAppearance(hwnd, message, wParam)
	case wmClose:
		_, _, _ = procDestroyWindow.Call(hwnd)
		return 0
	case wmDestroy:
		_, _, _ = procPostQuitMessage.Call(0)
		return 0
	}
	result, _, _ := procDefWindowProcW.Call(hwnd, message, wParam, lParam)
	return result
}

func notifyAppearance(hwnd, message, wParam uintptr) {
	if message == wmSettingChange && !appearanceSetting(wParam) {
		return
	}
	changed, _ := appearanceWatchers.Load(hwnd)
	if f, ok := changed.(func()); ok {
		f()
	}
}

func appearanceSetting(wParam uintptr) bool {
	return wParam == settingChangeImmersiveArea || wParam == spiSetHighContrast || wParam == spiSetClientAreaAnimation
}

func pumpMessages() error {
	var msg windowMessage
	for {
		result, _, err := procGetMessageW.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0) //nolint:gosec // GetMessageW writes one MSG to the live stack variable
		switch result & 0xffffffff {
		case 0:
			return nil
		case 0xffffffff:
			return fmt.Errorf("read the appearance watcher messages: %w", err)
		}
		_, _, _ = procDispatchMessageW.Call(uintptr(unsafe.Pointer(&msg))) //nolint:gosec // DispatchMessageW reads the MSG returned by GetMessageW
	}
}

func systemParameter(action uint32, value unsafe.Pointer) error {
	size := uintptr(0)
	if action == spiGetHighContrast {
		size = unsafe.Sizeof(highContrast{})
	}
	ok, _, err := procSystemParametersInfo.Call(uintptr(action), size, uintptr(value), 0)
	if ok == 0 {
		return err
	}
	return nil
}
