//go:build windows

package auth_test

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/auth"
)

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

func TestProtectHomeTreeGrantsOnlyPrivatePrincipals(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("ProtectHomeTree: %v", err)
	}
	child := filepath.Join(home, "auth.json")
	if err := os.WriteFile(child, []byte("{}"), 0o600); err != nil {
		t.Fatalf("write child: %v", err)
	}
	if err := auth.CheckHomeDir(home); err != nil {
		t.Fatalf("CheckHomeDir home: %v", err)
	}
	assertChildInheritedPrivate(t, child)
}

func TestCheckHomeDirRefusesEveryone(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("ProtectHomeTree: %v", err)
	}
	grantEveryoneAccess(t, home)
	refused := auth.CheckHomeDir(home)
	if refused == nil || !strings.Contains(refused.Error(), "S-1-1-0") {
		t.Fatalf("CheckHomeDir = %v, want refusal naming S-1-1-0", refused)
	}
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("reapply: %v", err)
	}
	if err := auth.CheckHomeDir(home); err != nil {
		t.Fatalf("CheckHomeDir after reapply: %v", err)
	}
}

func TestProtectHomeTreeRemovesExplicitChildEntry(t *testing.T) {
	home := filepath.Join(t.TempDir(), "home")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatalf("MkdirAll home: %v", err)
	}
	child := filepath.Join(home, "auth.json")
	if err := os.WriteFile(child, []byte("{}"), 0o600); err != nil {
		t.Fatalf("write child: %v", err)
	}
	grantEveryoneAccess(t, child)
	assertChildHasExplicitEveryone(t, child)
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("ProtectHomeTree: %v", err)
	}
	if err := auth.CheckHomeDir(home); err != nil {
		t.Fatalf("CheckHomeDir home: %v", err)
	}
	assertChildProtectedPrivate(t, child)
}

func fileSDDL(t *testing.T, path string) string {
	t.Helper()
	sd, sdErr := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION)
	if sdErr != nil {
		t.Fatalf("security info: %v", sdErr)
	}
	sddl := sd.String()
	if sddl == "" {
		t.Fatal("empty security descriptor")
	}
	return sddl
}

func childDACLSDDL(t *testing.T, path string) string {
	t.Helper()
	sd, sdErr := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if sdErr != nil {
		t.Fatalf("security info: %v", sdErr)
	}
	sddl := sd.String()
	if sddl == "" {
		t.Fatal("empty security descriptor")
	}
	return sddl
}

func childACEEntries(t *testing.T, sddl string) []string {
	t.Helper()
	_, section, found := strings.Cut(sddl, "D:")
	if !found {
		t.Fatalf("no DACL in %q", sddl)
	}
	_, body, found := strings.Cut(section, "(")
	if !found {
		t.Fatalf("no DACL entries in %q", sddl)
	}
	rest, _, _ := strings.Cut(body, "S:")
	parts := strings.Split(rest, ")(")
	entries := make([]string, 0, len(parts))
	for _, part := range parts {
		part, _ = strings.CutPrefix(part, "(")
		part, _ = strings.CutSuffix(part, ")")
		if len(strings.SplitN(part, ";", 7)) < 6 {
			t.Fatalf("malformed ACE %q in %q", part, sddl)
		}
		entries = append(entries, part)
	}
	if len(entries) == 0 {
		t.Fatalf("no DACL entries in %q", sddl)
	}
	return entries
}

func childACEFlagsTrustee(entry string) (string, string) {
	fields := strings.SplitN(entry, ";", 7)
	return fields[1], fields[5]
}

func assertChildInheritedPrivate(t *testing.T, path string) {
	t.Helper()
	for _, entry := range childACEEntries(t, childDACLSDDL(t, path)) {
		flags, trustee := childACEFlagsTrustee(entry)
		if !strings.Contains(flags, "ID") {
			t.Fatalf("explicit ACE %q on %s", entry, path)
		}
		if trustee == "WD" || trustee == "S-1-1-0" {
			t.Fatalf("Everyone ACE %q on %s", entry, path)
		}
	}
}

func assertChildHasExplicitEveryone(t *testing.T, path string) {
	t.Helper()
	for _, entry := range childACEEntries(t, childDACLSDDL(t, path)) {
		flags, trustee := childACEFlagsTrustee(entry)
		if strings.Contains(flags, "ID") {
			continue
		}
		if trustee == "WD" || trustee == "S-1-1-0" {
			return
		}
	}
	t.Fatalf("no explicit Everyone entry on %s", path)
}

func assertChildProtectedPrivate(t *testing.T, path string) {
	t.Helper()
	sddl := childDACLSDDL(t, path)
	_, tail, found := strings.Cut(sddl, "D:")
	if !found {
		t.Fatalf("no DACL in %q", sddl)
	}
	head, _, _ := strings.Cut(tail, "(")
	if !strings.Contains(head, "P") {
		t.Fatalf("child DACL inherits on %s: %q", path, sddl)
	}
	for _, entry := range childACEEntries(t, sddl) {
		_, trustee := childACEFlagsTrustee(entry)
		if trustee == "WD" || trustee == "S-1-1-0" {
			t.Fatalf("Everyone ACE %q on %s", entry, path)
		}
	}
}

func TestProtectHomeTreeSkipsOutsideSymlink(t *testing.T) {
	root := t.TempDir()
	home := filepath.Join(root, "home")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatalf("MkdirAll home: %v", err)
	}
	outside := filepath.Join(root, "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatalf("MkdirAll outside: %v", err)
	}
	witness := filepath.Join(outside, "witness.txt")
	if err := os.WriteFile(witness, []byte("witness"), 0o600); err != nil {
		t.Fatalf("write witness: %v", err)
	}
	before := fileSDDL(t, witness)
	link := filepath.Join(home, "link")
	if err := os.Symlink(outside, link); err != nil {
		t.Skipf("cannot create symlinks: %v", err)
	}
	dangling := filepath.Join(home, "dangling")
	if err := os.Symlink(filepath.Join(root, "missing"), dangling); err != nil {
		t.Skipf("cannot create symlinks: %v", err)
	}
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("ProtectHomeTree: %v", err)
	}
	if after := fileSDDL(t, witness); after != before {
		t.Fatalf("outside DACL changed:\nbefore %s\nafter  %s", before, after)
	}
}
