//go:build windows

package github

import (
	"errors"
	"fmt"
	"io"
	"os"
	"unsafe"

	"golang.org/x/sys/windows"
)

const maxPrivateKeyBytes = 65536

var errPrivateKeyNotPrivate = errors.New("github: private key file must be private to the current user")

func readPrivateKeyFile(path string) ([]byte, error) {
	p, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, fmt.Errorf("github: open private key: %w", err)
	}
	h, err := windows.CreateFile(p, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_OPEN_REPARSE_POINT|windows.FILE_FLAG_BACKUP_SEMANTICS, 0)
	if err != nil {
		return nil, fmt.Errorf("github: open private key: %w", err)
	}
	defer func() { _ = windows.CloseHandle(h) }()

	var attrs uint32
	if err := windows.GetFileInformationByHandleEx(h, windows.FileAttributeTagInfo, (*byte)(unsafe.Pointer(&attrs)), uint32(unsafe.Sizeof(attrs))); err != nil {
		return nil, fmt.Errorf("github: stat private key: %w", err)
	}
	if attrs&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
		return nil, errPrivateKeyNotPrivate
	}

	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(h, &info); err != nil {
		return nil, fmt.Errorf("github: stat private key: %w", err)
	}
	if info.FileAttributes&windows.FILE_ATTRIBUTE_DIRECTORY != 0 {
		return nil, errPrivateKeyNotPrivate
	}
	if err := checkPrivateKeyACL(h); err != nil {
		return nil, err
	}

	file := os.NewFile(uintptr(h), path)
	data, err := io.ReadAll(io.LimitReader(file, maxPrivateKeyBytes+1))
	_ = file.Close()
	if err != nil {
		return nil, fmt.Errorf("github: read private key: %w", err)
	}
	if len(data) > maxPrivateKeyBytes {
		return nil, errors.New("github: private key file exceeds 65536 bytes")
	}
	return data, nil
}

func checkPrivateKeyACL(h windows.Handle) error {
	var sd *windows.SECURITY_DESCRIPTOR
	if err := windows.GetSecurityInfo(h, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, &sd, nil); err != nil {
		return fmt.Errorf("github: inspect private key permissions: %w", err)
	}
	if sd == nil {
		return errPrivateKeyNotPrivate
	}
	daclPresent, daclDefaulted, err := sd.DACL()
	if err != nil {
		return fmt.Errorf("github: inspect private key permissions: %w", err)
	}
	if !daclPresent || daclDefaulted {
		return errPrivateKeyNotPrivate
	}
	acl, err := sd.ACL()
	if err != nil {
		return fmt.Errorf("github: inspect private key permissions: %w", err)
	}
	if acl == nil || acl.AceCount == 0 {
		return errPrivateKeyNotPrivate
	}
	allowed := allowedKeyACLPrincipals()
	for i := uint32(0); i < acl.AceCount; i++ {
		var acePtr *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(acl, i, (*unsafe.Pointer)(unsafe.Pointer(&acePtr))); err != nil {
			return fmt.Errorf("github: inspect private key permissions: %w", err)
		}
		if acePtr == nil {
			return errPrivateKeyNotPrivate
		}
		sid := (*windows.SID)(unsafe.Pointer(&acePtr.SidStart))
		if !allowed(sid) {
			return errPrivateKeyNotPrivate
		}
	}
	return nil
}

func allowedKeyACLPrincipals() func(*windows.SID) bool {
	var sids []*windows.SID
	if user, err := currentUserSIDWindows(); err == nil {
		sids = append(sids, user)
	}
	if system, err := windows.CreateWellKnownSid(windows.WinLocalSystemSid); err == nil {
		sids = append(sids, system)
	}
	if admins, err := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid); err == nil {
		sids = append(sids, admins)
	}
	return func(sid *windows.SID) bool {
		for _, candidate := range sids {
			if candidate != nil && windows.EqualSid(sid, candidate) {
				return true
			}
		}
		return false
	}
}

func currentUserSIDWindows() (*windows.SID, error) {
	process := windows.CurrentProcess()
	token := windows.Token(0)
	if err := windows.OpenProcessToken(process, windows.TOKEN_QUERY, &token); err != nil {
		return nil, fmt.Errorf("github: identify current user: %w", err)
	}
	defer func() { _ = windows.CloseHandle(token) }()
	var size uint32
	if err := windows.GetTokenInformation(token, windows.TokenUser, nil, 0, &size); err != windows.ERROR_INSUFFICIENT_BUFFER {
		return nil, fmt.Errorf("github: identify current user: %w", err)
	}
	buf := make([]byte, size)
	if err := windows.GetTokenInformation(token, windows.TokenUser, &buf[0], size, &size); err != nil {
		return nil, fmt.Errorf("github: identify current user: %w", err)
	}
	return (*windows.Tokenuser)(unsafe.Pointer(&buf[0])).User.Sid, nil
}
