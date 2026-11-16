//go:build windows

package runner

import (
	"errors"
	"fmt"
	"runtime"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	fileDeleteChild                     = 0x40
	rootGrantMask   windows.ACCESS_MASK = windows.FILE_GENERIC_READ | windows.FILE_GENERIC_WRITE | windows.FILE_GENERIC_EXECUTE | windows.DELETE | fileDeleteChild
)

func grantRoots(grants []rootCapability) error {
	for _, grant := range grants {
		if err := grantRoot(grant.root, grant.sid); err != nil {
			return err
		}
	}
	return nil
}

func grantRoot(root string, sid *windows.SID) error {
	handle, inheritance, err := openACLTarget(root)
	if err != nil {
		return err
	}
	defer func() { _ = windows.CloseHandle(handle) }()
	descriptor, err := windows.GetSecurityInfo(handle, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return fmt.Errorf("runner: read ACL of %s: %w", root, err)
	}
	return mergeRootGrant(handle, root, descriptor, sid, inheritance)
}

func mergeRootGrant(handle windows.Handle, root string, descriptor *windows.SECURITY_DESCRIPTOR, sid *windows.SID, inheritance uint32) error {
	dacl, _, err := descriptor.DACL()
	if errors.Is(err, windows.ERROR_OBJECT_NOT_FOUND) || (err == nil && dacl == nil) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("runner: read DACL of %s: %w", root, err)
	}
	if daclGrants(dacl, sid, inheritance) {
		runtime.KeepAlive(descriptor)
		return nil
	}
	merged, err := windows.ACLFromEntries([]windows.EXPLICIT_ACCESS{rootAccess(sid, inheritance)}, dacl)
	runtime.KeepAlive(descriptor)
	runtime.KeepAlive(sid)
	if err != nil {
		return fmt.Errorf("runner: build ACL of %s: %w", root, err)
	}
	if err := windows.SetSecurityInfo(handle, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION, nil, nil, merged, nil); err != nil {
		return fmt.Errorf("runner: apply ACL of %s: %w", root, err)
	}
	return nil
}

func rootAccess(sid *windows.SID, inheritance uint32) windows.EXPLICIT_ACCESS {
	return windows.EXPLICIT_ACCESS{
		AccessPermissions: rootGrantMask,
		AccessMode:        windows.GRANT_ACCESS,
		Inheritance:       inheritance,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  windows.TRUSTEE_IS_GROUP,
			TrusteeValue: windows.TrusteeValueFromSID(sid),
		},
	}
}

func daclGrants(dacl *windows.ACL, sid *windows.SID, inheritance uint32) bool {
	for index := range uint32(dacl.AceCount) {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(dacl, index, &ace); err != nil {
			return false
		}
		if aceGrants(ace, sid, inheritance) {
			return true
		}
	}
	return false
}

func aceGrants(ace *windows.ACCESS_ALLOWED_ACE, sid *windows.SID, inheritance uint32) bool {
	if ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE {
		return false
	}
	if ace.Header.AceFlags&(windows.INHERITED_ACE|windows.INHERIT_ONLY_ACE) != 0 {
		return false
	}
	if ace.Mask&rootGrantMask != rootGrantMask {
		return false
	}
	if ace.Header.AceFlags&uint8(inheritance) != uint8(inheritance) {
		return false
	}
	return (*windows.SID)(unsafe.Pointer(&ace.SidStart)).Equals(sid)
}

func openACLTarget(path string) (windows.Handle, uint32, error) {
	encoded, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return 0, 0, fmt.Errorf("runner: encode ACL target %s: %w", path, err)
	}
	handle, err := windows.CreateFile(
		encoded,
		windows.READ_CONTROL|windows.WRITE_DAC,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE,
		nil,
		windows.OPEN_EXISTING,
		windows.FILE_FLAG_BACKUP_SEMANTICS|windows.FILE_FLAG_OPEN_REPARSE_POINT,
		0,
	)
	if err != nil {
		return 0, 0, fmt.Errorf("runner: open ACL target %s: %w", path, err)
	}
	if err := requirePersistentACLs(handle); err != nil {
		_ = windows.CloseHandle(handle)
		return 0, 0, err
	}
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(handle, &info); err != nil {
		_ = windows.CloseHandle(handle)
		return 0, 0, fmt.Errorf("runner: inspect ACL target %s: %w", path, err)
	}
	if info.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
		_ = windows.CloseHandle(handle)
		return 0, 0, fmt.Errorf("runner: writable root %s is a reparse point", path)
	}
	return handle, targetInheritance(info.FileAttributes), nil
}

func targetInheritance(attributes uint32) uint32 {
	if attributes&windows.FILE_ATTRIBUTE_DIRECTORY != 0 {
		return windows.SUB_CONTAINERS_AND_OBJECTS_INHERIT
	}
	return windows.NO_INHERITANCE
}

func requirePersistentACLs(handle windows.Handle) error {
	var flags uint32
	if err := windows.GetVolumeInformationByHandle(handle, nil, 0, nil, nil, &flags, nil, 0); err != nil {
		return fmt.Errorf("runner: inspect Windows filesystem capabilities: %w", err)
	}
	if flags&windows.FILE_PERSISTENT_ACLS == 0 {
		return errors.New("runner: Windows sandbox requires persistent filesystem ACLs")
	}
	return nil
}
