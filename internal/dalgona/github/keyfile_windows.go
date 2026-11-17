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

	var info windows.ByHandleFileInformation
	if infoErr := windows.GetFileInformationByHandle(h, &info); infoErr != nil {
		return nil, fmt.Errorf("github: stat private key: %w", infoErr)
	}
	if info.FileAttributes&(windows.FILE_ATTRIBUTE_REPARSE_POINT|windows.FILE_ATTRIBUTE_DIRECTORY) != 0 {
		return nil, errPrivateKeyNotPrivate
	}
	if aclErr := checkPrivateKeyACL(h); aclErr != nil {
		return nil, aclErr
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

func privateKeyDACL(h windows.Handle) (*windows.ACL, error) {
	sd, err := windows.GetSecurityInfo(h, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION)
	if err != nil {
		return nil, fmt.Errorf("github: inspect private key permissions: %w", err)
	}
	if sd == nil {
		return nil, errPrivateKeyNotPrivate
	}
	acl, _, err := sd.DACL()
	if errors.Is(err, windows.ERROR_OBJECT_NOT_FOUND) {
		return nil, errPrivateKeyNotPrivate
	}
	if err != nil {
		return nil, fmt.Errorf("github: inspect private key permissions: %w", err)
	}
	if acl == nil || acl.AceCount == 0 {
		return nil, errPrivateKeyNotPrivate
	}
	return acl, nil
}

func checkPrivateKeyACL(h windows.Handle) error {
	acl, err := privateKeyDACL(h)
	if err != nil {
		return err
	}
	allowed := allowedKeyACLPrincipals()
	for i := uint32(0); i < uint32(acl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(acl, i, &ace); err != nil {
			return fmt.Errorf("github: inspect private key permissions: %w", err)
		}
		if ace == nil {
			return errPrivateKeyNotPrivate
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart)) //nolint:gosec // SidStart marks the inline SID bytes of the ACE
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
	defer func() { _ = token.Close() }()
	var size uint32
	if err := windows.GetTokenInformation(token, windows.TokenUser, nil, 0, &size); !errors.Is(err, windows.ERROR_INSUFFICIENT_BUFFER) {
		return nil, fmt.Errorf("github: identify current user: %w", err)
	}
	buf := make([]byte, size)
	if err := windows.GetTokenInformation(token, windows.TokenUser, &buf[0], size, &size); err != nil {
		return nil, fmt.Errorf("github: identify current user: %w", err)
	}
	return (*windows.Tokenuser)(unsafe.Pointer(&buf[0])).User.Sid, nil //nolint:gosec // GetTokenInformation writes the TOKEN_USER struct into buf
}
