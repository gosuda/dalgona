//go:build windows

package runner

import (
	"errors"
	"fmt"
	"runtime"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	tokenDisableMaxPrivilege = 0x01
	tokenLUA                 = 0x04
	tokenWriteRestricted     = 0x08
	tokenOpenAccess          = windows.TOKEN_DUPLICATE | windows.TOKEN_QUERY | windows.TOKEN_ASSIGN_PRIMARY | windows.TOKEN_ADJUST_DEFAULT | windows.TOKEN_ADJUST_SESSIONID | windows.TOKEN_ADJUST_PRIVILEGES
)

type tokenDefaultDaclInfo struct {
	DefaultDacl *windows.ACL
}

func createRestrictedToken(sids []*windows.SID) (windows.Token, error) {
	if len(sids) == 0 {
		return 0, errors.New("runner: restricted token needs a capability SID")
	}
	var base windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), tokenOpenAccess, &base); err != nil {
		return 0, fmt.Errorf("runner: open process token: %w", err)
	}
	defer func() { _ = base.Close() }()
	return restrictToken(base, sids)
}

func restrictToken(base windows.Token, sids []*windows.SID) (windows.Token, error) {
	logon, err := logonSID(base)
	if err != nil {
		return 0, err
	}
	entries := make([]windows.SIDAndAttributes, 0, len(sids)+1)
	for _, sid := range sids {
		entries = append(entries, windows.SIDAndAttributes{Sid: sid})
	}
	entries = append(entries, windows.SIDAndAttributes{Sid: sidFromBytes(logon)})
	token, err := callCreateRestrictedToken(base, entries)
	runtime.KeepAlive(logon)
	runtime.KeepAlive(sids)
	if err != nil {
		return 0, err
	}
	if err := prepareRestrictedToken(token, logon); err != nil {
		_ = token.Close()
		return 0, err
	}
	return token, nil
}

func callCreateRestrictedToken(base windows.Token, entries []windows.SIDAndAttributes) (windows.Token, error) {
	procedure := windows.NewLazySystemDLL("advapi32.dll").NewProc("CreateRestrictedToken")
	flags := uintptr(tokenDisableMaxPrivilege | tokenLUA | tokenWriteRestricted)
	var restricted windows.Token
	result, _, callErr := procedure.Call(
		uintptr(base),
		flags,
		0,
		0,
		0,
		0,
		uintptr(len(entries)),
		uintptr(unsafe.Pointer(&entries[0])), //nolint:gosec // CreateRestrictedToken takes the SIDAndAttributes slice as a raw pointer
		uintptr(unsafe.Pointer(&restricted)), //nolint:gosec // CreateRestrictedToken writes the token handle through a raw pointer
	)
	runtime.KeepAlive(entries)
	if result != 0 {
		return restricted, nil
	}
	if !errors.Is(callErr, syscall.Errno(0)) {
		return 0, fmt.Errorf("runner: CreateRestrictedToken: %w", callErr)
	}
	return 0, errors.New("runner: CreateRestrictedToken failed")
}

func prepareRestrictedToken(token windows.Token, logon []byte) error {
	if err := enableTokenPrivilege(token, "SeChangeNotifyPrivilege"); err != nil {
		return err
	}
	return broadenTokenDefaultDacl(token, logon)
}

func broadenTokenDefaultDacl(token windows.Token, logon []byte) error {
	current, buffer, err := tokenDefaultDacl(token)
	if err != nil {
		return err
	}
	access := windows.EXPLICIT_ACCESS{
		AccessPermissions: windows.GENERIC_READ | windows.GENERIC_WRITE,
		AccessMode:        windows.GRANT_ACCESS,
		Inheritance:       windows.NO_INHERITANCE,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  windows.TRUSTEE_IS_USER,
			TrusteeValue: windows.TrusteeValueFromSID(sidFromBytes(logon)),
		},
	}
	merged, err := windows.ACLFromEntries([]windows.EXPLICIT_ACCESS{access}, current)
	runtime.KeepAlive(buffer)
	runtime.KeepAlive(logon)
	if err != nil {
		return fmt.Errorf("runner: merge token default DACL: %w", err)
	}
	return setTokenDefaultDacl(token, merged)
}

func tokenDefaultDacl(token windows.Token) (*windows.ACL, []byte, error) {
	var size uint32
	err := windows.GetTokenInformation(token, windows.TokenDefaultDacl, nil, 0, &size)
	if !errors.Is(err, windows.ERROR_INSUFFICIENT_BUFFER) {
		return nil, nil, fmt.Errorf("runner: size token default DACL: %w", err)
	}
	buffer := make([]byte, size)
	if err := windows.GetTokenInformation(token, windows.TokenDefaultDacl, &buffer[0], size, &size); err != nil {
		return nil, nil, fmt.Errorf("runner: read token default DACL: %w", err)
	}
	info := (*tokenDefaultDaclInfo)(unsafe.Pointer(&buffer[0])) //nolint:gosec // GetTokenInformation writes the TOKEN_DEFAULT_DACL struct into buffer
	return info.DefaultDacl, buffer, nil
}

func setTokenDefaultDacl(token windows.Token, dacl *windows.ACL) error {
	info := tokenDefaultDaclInfo{DefaultDacl: dacl}
	err := windows.SetTokenInformation(token, windows.TokenDefaultDacl, (*byte)(unsafe.Pointer(&info)), uint32(unsafe.Sizeof(info))) //nolint:gosec // SetTokenInformation takes the info struct as a raw pointer
	runtime.KeepAlive(info)
	runtime.KeepAlive(dacl)
	if err != nil {
		return fmt.Errorf("runner: set token default DACL: %w", err)
	}
	return nil
}

func logonSID(token windows.Token) ([]byte, error) {
	sid, err := tokenLogonSID(token)
	if err == nil {
		return sid, nil
	}
	linked, linkedErr := token.GetLinkedToken()
	if linkedErr != nil {
		return nil, err
	}
	defer func() { _ = linked.Close() }()
	return tokenLogonSID(linked)
}

func tokenLogonSID(token windows.Token) ([]byte, error) {
	groups, err := token.GetTokenGroups()
	if err != nil {
		return nil, fmt.Errorf("runner: read token groups: %w", err)
	}
	defer runtime.KeepAlive(groups)
	for _, group := range groups.AllGroups() {
		if group.Attributes&windows.SE_GROUP_LOGON_ID == windows.SE_GROUP_LOGON_ID {
			return copySID(group.Sid)
		}
	}
	return nil, errors.New("runner: logon SID not present on token")
}

func copySID(sid *windows.SID) ([]byte, error) {
	length := windows.GetLengthSid(sid)
	if length == 0 {
		return nil, errors.New("runner: invalid SID length")
	}
	out := make([]byte, length)
	if err := windows.CopySid(length, sidFromBytes(out), sid); err != nil {
		return nil, fmt.Errorf("runner: copy SID: %w", err)
	}
	return out, nil
}

func sidFromBytes(value []byte) *windows.SID {
	return (*windows.SID)(unsafe.Pointer(&value[0])) //nolint:gosec // value holds marshalled SID bytes
}

func enableTokenPrivilege(token windows.Token, name string) error {
	encoded, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return fmt.Errorf("runner: encode token privilege %s: %w", name, err)
	}
	var luid windows.LUID
	if err := windows.LookupPrivilegeValue(nil, encoded, &luid); err != nil {
		return fmt.Errorf("runner: look up token privilege %s: %w", name, err)
	}
	privileges := windows.Tokenprivileges{PrivilegeCount: 1}
	privileges.Privileges[0] = windows.LUIDAndAttributes{Luid: luid, Attributes: windows.SE_PRIVILEGE_ENABLED}
	if err := windows.AdjustTokenPrivileges(token, false, &privileges, 0, nil, nil); err != nil {
		return fmt.Errorf("runner: enable token privilege %s: %w", name, err)
	}
	return nil
}
