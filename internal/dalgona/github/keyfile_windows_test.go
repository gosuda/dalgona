//go:build windows

package github

import (
	"os"
	"os/user"
	"testing"

	"golang.org/x/sys/windows"
)

func setTestKeyACL(t *testing.T, path string, perm os.FileMode) {
	t.Helper()
	current, err := user.Current()
	if err != nil {
		t.Fatalf("current user: %v", err)
	}
	owner, err := windows.StringToSid(current.Uid)
	if err != nil {
		t.Fatalf("owner sid: %v", err)
	}
	system, err := windows.CreateWellKnownSid(windows.WinLocalSystemSid)
	if err != nil {
		t.Fatalf("system sid: %v", err)
	}
	admins, err := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid)
	if err != nil {
		t.Fatalf("admins sid: %v", err)
	}
	entries := []windows.EXPLICIT_ACCESS{
		keyAccess(owner, windows.TRUSTEE_IS_USER, windows.GENERIC_ALL),
		keyAccess(system, windows.TRUSTEE_IS_WELL_KNOWN_GROUP, windows.GENERIC_ALL),
		keyAccess(admins, windows.TRUSTEE_IS_WELL_KNOWN_GROUP, windows.GENERIC_ALL),
	}
	if perm&0o077 != 0 {
		users, usersErr := windows.CreateWellKnownSid(windows.WinBuiltinUsersSid)
		if usersErr != nil {
			t.Fatalf("users sid: %v", usersErr)
		}
		entries = append(entries, keyAccess(users, windows.TRUSTEE_IS_WELL_KNOWN_GROUP, windows.GENERIC_READ))
	}
	acl, err := windows.ACLFromEntries(entries, nil)
	if err != nil {
		t.Fatalf("build acl: %v", err)
	}
	if err := windows.SetNamedSecurityInfo(path, windows.SE_FILE_OBJECT,
		windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION,
		nil, nil, acl, nil); err != nil {
		t.Fatalf("set acl: %v", err)
	}
}

func keyAccess(sid *windows.SID, trusteeType windows.TRUSTEE_TYPE, access windows.ACCESS_MASK) windows.EXPLICIT_ACCESS {
	return windows.EXPLICIT_ACCESS{
		AccessPermissions: access,
		AccessMode:        windows.GRANT_ACCESS,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  trusteeType,
			TrusteeValue: windows.TrusteeValueFromSID(sid),
		},
	}
}
