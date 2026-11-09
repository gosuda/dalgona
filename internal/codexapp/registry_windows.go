//go:build windows

package codexapp

import (
	"errors"
	"fmt"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

func registryValue() (string, bool, error) {
	key, err := registry.OpenKey(registry.CURRENT_USER, `Environment`, registry.QUERY_VALUE)
	if err != nil {
		if errors.Is(err, registry.ErrNotExist) {
			return "", false, nil
		}
		return "", false, fmt.Errorf("codexapp: open environment registry key: %w", err)
	}
	defer key.Close()
	value, _, err := key.GetStringValue("CODEX_CLI_PATH")
	if err != nil {
		if errors.Is(err, registry.ErrNotExist) {
			return "", false, nil
		}
		return "", false, fmt.Errorf("codexapp: read CODEX_CLI_PATH: %w", err)
	}
	return value, true, nil
}

func setRegistry(value string) error {
	key, _, err := registry.CreateKey(registry.CURRENT_USER, `Environment`, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("codexapp: open environment registry key: %w", err)
	}
	defer key.Close()
	if err := key.SetStringValue("CODEX_CLI_PATH", value); err != nil {
		return fmt.Errorf("codexapp: set CODEX_CLI_PATH: %w", err)
	}
	return broadcastEnvironmentChange()
}

func restoreRegistry(value string, found bool) error {
	key, _, err := registry.CreateKey(registry.CURRENT_USER, `Environment`, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("codexapp: open environment registry key: %w", err)
	}
	defer key.Close()
	if found {
		if err := key.SetStringValue("CODEX_CLI_PATH", value); err != nil {
			return fmt.Errorf("codexapp: restore CODEX_CLI_PATH: %w", err)
		}
	} else if err := key.DeleteValue("CODEX_CLI_PATH"); err != nil && !errors.Is(err, registry.ErrNotExist) {
		return fmt.Errorf("codexapp: remove CODEX_CLI_PATH: %w", err)
	}
	return broadcastEnvironmentChange()
}

func broadcastEnvironmentChange() error {
	dll := windows.NewLazySystemDLL("user32.dll")
	proc := dll.NewProc("SendMessageTimeoutW")
	setting, err := windows.UTF16PtrFromString("Environment")
	if err != nil {
		return fmt.Errorf("codexapp: encode environment broadcast: %w", err)
	}
	const (
		hwndBroadcast   = 0xffff
		wmSettingChange = 0x001a
		smtoAbortIfHung = 0x0002
	)
	var result uintptr
	_, _, callErr := proc.Call(hwndBroadcast, wmSettingChange, 0, uintptr(unsafe.Pointer(setting)), smtoAbortIfHung, 5000, uintptr(unsafe.Pointer(&result))) //nolint:gosec // SendMessageTimeoutW needs the string and result pointers passed as uintptr
	if !errors.Is(callErr, windows.ERROR_SUCCESS) && !errors.Is(callErr, windows.ERROR_TIMEOUT) {
		return fmt.Errorf("codexapp: broadcast environment change: %w", callErr)
	}
	return nil
}
