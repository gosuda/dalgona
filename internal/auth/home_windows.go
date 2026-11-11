//go:build windows

package auth

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"

	"golang.org/x/sys/windows"
)

func ProtectHomeTree(dir string) error {
	acl, aclErr := homeRootACL(dir)
	if aclErr != nil {
		return aclErr
	}
	return propagateHomeACL(dir, acl)
}

func ProtectHomeDir(dir string) error {
	_, err := homeRootACL(dir)
	return err
}

func homeRootACL(dir string) (*windows.ACL, error) {
	if dir == "" {
		return nil, errors.New("auth: home directory is empty")
	}
	if mkdirErr := os.MkdirAll(dir, 0o700); mkdirErr != nil {
		return nil, mkdirErr
	}
	acl, aclErr := homeACL()
	if aclErr != nil {
		return nil, aclErr
	}
	if rootErr := protectHomeRoot(dir, acl); rootErr != nil {
		return nil, rootErr
	}
	return acl, nil
}

func protectHomeRoot(dir string, acl *windows.ACL) error {
	applyErr := windows.SetNamedSecurityInfo(dir, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, acl, nil)
	if applyErr == nil {
		return nil
	}
	if isAccessControlUnsupported(applyErr) {
		return fmt.Errorf("auth: home %s is on a file system that does not support access control: %w", dir, applyErr)
	}
	return fmt.Errorf("auth: protect home %s: %w", dir, applyErr)
}

func isAccessControlUnsupported(applyErr error) bool {
	errno, ok := errors.AsType[windows.Errno](applyErr)
	if !ok {
		return false
	}
	return errno == windows.ERROR_INVALID_PARAMETER || errno == windows.ERROR_NOT_SUPPORTED || errno == windows.ERROR_INVALID_FUNCTION
}

func CheckHomeDir(dir string) error {
	sd, sdErr := windows.GetNamedSecurityInfo(dir, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if sdErr != nil {
		return sdErr
	}
	user, userErr := currentUserSID()
	if userErr != nil {
		return userErr
	}
	system, systemErr := windows.CreateWellKnownSid(windows.WinLocalSystemSid)
	if systemErr != nil {
		return systemErr
	}
	admins, adminsErr := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid)
	if adminsErr != nil {
		return adminsErr
	}
	return checkHomeSDDLAllow(dir, sd.String(), homeTrusteeAllow(user, system, admins))
}

func homeTrusteeAllow(user, system, admins *windows.SID) func(string) (bool, string) {
	allowed := []*windows.SID{user, system, admins}
	return func(principal string) (bool, string) {
		sid, sidErr := sddlTrusteeSID(principal, user, system, admins)
		if sidErr != nil {
			return false, principal
		}
		for _, want := range allowed {
			if sid.Equals(want) {
				return true, ""
			}
		}
		return false, sid.String()
	}
}

func sddlTrusteeSID(principal string, user, system, admins *windows.SID) (*windows.SID, error) {
	switch principal {
	case "LA":
		if user.IsWellKnown(windows.WinAccountAdministratorSid) {
			return user, nil
		}
		return nil, errors.New("auth: home grants access to a foreign administrator")
	case "SY":
		return system, nil
	case "BA":
		return admins, nil
	case "WD":
		return windows.CreateWellKnownSid(windows.WinWorldSid)
	default:
		return windows.StringToSid(principal)
	}
}

func currentUserSID() (*windows.SID, error) {
	info, infoErr := windows.GetCurrentProcessToken().GetTokenUser()
	if infoErr != nil {
		return nil, infoErr
	}
	return info.User.Sid.Copy()
}

func homeACL() (*windows.ACL, error) {
	user, userErr := currentUserSID()
	if userErr != nil {
		return nil, userErr
	}
	system, systemErr := windows.CreateWellKnownSid(windows.WinLocalSystemSid)
	if systemErr != nil {
		return nil, systemErr
	}
	admins, adminsErr := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid)
	if adminsErr != nil {
		return nil, adminsErr
	}
	var pinner runtime.Pinner
	pinner.Pin(user)
	pinner.Pin(system)
	pinner.Pin(admins)
	defer pinner.Unpin()
	entries := []windows.EXPLICIT_ACCESS{
		grantHomeAccess(user, windows.TRUSTEE_IS_USER),
		grantHomeAccess(system, windows.TRUSTEE_IS_WELL_KNOWN_GROUP),
		grantHomeAccess(admins, windows.TRUSTEE_IS_WELL_KNOWN_GROUP),
	}
	acl, aclErr := windows.ACLFromEntries(entries, nil)
	if aclErr != nil {
		return nil, aclErr
	}
	return acl, nil
}

func grantHomeAccess(sid *windows.SID, trusteeType windows.TRUSTEE_TYPE) windows.EXPLICIT_ACCESS {
	return windows.EXPLICIT_ACCESS{
		AccessPermissions: windows.GENERIC_ALL,
		AccessMode:        windows.GRANT_ACCESS,
		Inheritance:       windows.OBJECT_INHERIT_ACE | windows.CONTAINER_INHERIT_ACE,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  trusteeType,
			TrusteeValue: windows.TrusteeValueFromSID(sid),
		},
	}
}

func propagateHomeACL(home string, acl *windows.ACL) error {
	walkErr := filepath.WalkDir(home, func(path string, entry os.DirEntry, entryErr error) error {
		return protectHomeChild(home, path, entry, acl, entryErr)
	})
	if walkErr != nil {
		return walkErr
	}
	return nil
}

func protectHomeChild(home, path string, entry os.DirEntry, acl *windows.ACL, entryErr error) error {
	if entryErr != nil {
		return entryErr
	}
	if path == home {
		return nil
	}
	if entry.Type()&fs.ModeSymlink != 0 || entry.Type()&fs.ModeIrregular != 0 {
		return nil
	}
	if applyErr := windows.SetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, acl, nil); applyErr != nil {
		return fmt.Errorf("auth: protect home child %s: %w", path, applyErr)
	}
	return nil
}
