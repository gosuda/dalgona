//go:build windows

package daemon

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"golang.org/x/sys/windows"
)

func TestDialPathAcceptsUpgradedHome(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatalf("MkdirAll: %v", err)
	}
	if err := tightenHomeRoot(home); err != nil {
		t.Fatalf("tightenHomeRoot: %v", err)
	}
	grantEveryoneAccess(t, home)
	refused := VerifyHome(home)
	if refused == nil || !strings.Contains(refused.Error(), "S-1-1-0") {
		t.Fatalf("VerifyHome with legacy Everyone grant = %v, want refusal naming S-1-1-0", refused)
	}
	if err := tightenHomeRoot(home); err != nil {
		t.Fatalf("tightenHomeRoot on upgraded home: %v", err)
	}
	if err := VerifyHome(home); err != nil {
		t.Fatalf("VerifyHome after re-tighten: %v", err)
	}
}

func grantEveryoneAccess(t *testing.T, path string) {
	t.Helper()
	sd, sdErr := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if sdErr != nil {
		t.Fatalf("security info: %v", sdErr)
	}
	dacl, _, daclErr := sd.DACL()
	if daclErr != nil {
		t.Fatalf("DACL: %v", daclErr)
	}
	everyone, sidErr := windows.StringToSid("S-1-1-0")
	if sidErr != nil {
		t.Fatalf("everyone sid: %v", sidErr)
	}
	var pinner runtime.Pinner
	pinner.Pin(everyone)
	defer pinner.Unpin()
	entry := windows.EXPLICIT_ACCESS{
		AccessPermissions: windows.GENERIC_ALL,
		AccessMode:        windows.GRANT_ACCESS,
		Inheritance:       windows.OBJECT_INHERIT_ACE | windows.CONTAINER_INHERIT_ACE,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  windows.TRUSTEE_IS_WELL_KNOWN_GROUP,
			TrusteeValue: windows.TrusteeValueFromSID(everyone),
		},
	}
	merged, aclErr := windows.ACLFromEntries([]windows.EXPLICIT_ACCESS{entry}, dacl)
	if aclErr != nil {
		t.Fatalf("merge acl: %v", aclErr)
	}
	if applyErr := windows.SetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION, nil, nil, merged, nil); applyErr != nil {
		t.Fatalf("grant everyone: %v", applyErr)
	}
}
