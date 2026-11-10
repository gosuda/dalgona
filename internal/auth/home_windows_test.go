//go:build windows

package auth_test

import (
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/auth"
)

func childAllowPrincipals(t *testing.T, path string) []string {
	t.Helper()
	sd, sdErr := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION)
	if sdErr != nil {
		t.Fatalf("security info: %v", sdErr)
	}
	sddl := sd.String()
	if sddl == "" {
		t.Fatal("empty security descriptor")
	}
	_, section, _ := strings.Cut(sddl, "D:")
	_, body, found := strings.Cut(section, "(")
	if !found {
		t.Fatalf("no DACL entries in %q", sddl)
	}
	rest, _, _ := strings.Cut(body, "S:")
	var out []string
	for part := range strings.SplitSeq(rest, ")(") {
		entry, _ := strings.CutPrefix(part, "(")
		entry, _ = strings.CutSuffix(entry, ")")
		fields := strings.SplitN(entry, ";", 7)
		if len(fields) < 6 {
			t.Fatalf("malformed ACE %q in %q", entry, sddl)
		}
		switch fields[0] {
		case "A", "OA", "XA", "ZA":
			out = append(out, fields[5])
		case "D", "OD", "XD", "AU", "AL", "OU", "OL", "ML", "RA", "SP", "XU", "TL", "FL":
		default:
			t.Fatalf("unknown ACE type %q in %q", fields[0], sddl)
		}
	}
	return out
}

func principalAllowed(principal, user string) bool {
	switch principal {
	case "S-1-5-18", "SY", "S-1-5-32-544", "BA":
		return true
	default:
		return principal == user
	}
}

func canonicalPrincipal(principal string) string {
	switch principal {
	case "SY":
		return "S-1-5-18"
	case "BA":
		return "S-1-5-32-544"
	case "WD":
		return "S-1-1-0"
	default:
		return principal
	}
}

func processUserSIDString(t *testing.T) string {
	t.Helper()
	var token windows.Token
	if openErr := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY, &token); openErr != nil {
		t.Fatalf("open token: %v", openErr)
	}
	defer func() { _ = token.Close() }()
	info, infoErr := token.GetTokenUser()
	if infoErr != nil {
		t.Fatalf("token user: %v", infoErr)
	}
	sid, sidErr := info.User.Sid.Copy()
	if sidErr != nil {
		t.Fatalf("copy sid: %v", sidErr)
	}
	return sid.String()
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
	user := processUserSIDString(t)
	var seen []string
	for _, principal := range childAllowPrincipals(t, child) {
		if !principalAllowed(principal, user) {
			t.Fatalf("child grants access to %s", principal)
		}
		seen = append(seen, canonicalPrincipal(principal))
	}
	for _, want := range []string{user, "S-1-5-18", "S-1-5-32-544"} {
		if !slices.Contains(seen, want) {
			t.Fatalf("child ACL %v misses %s", seen, want)
		}
	}
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
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("ProtectHomeTree: %v", err)
	}
	child := filepath.Join(home, "auth.json")
	if err := os.WriteFile(child, []byte("{}"), 0o600); err != nil {
		t.Fatalf("write child: %v", err)
	}
	grantEveryoneAccess(t, child)
	polluted := childAllowPrincipals(t, child)
	if !slices.ContainsFunc(polluted, func(principal string) bool { return canonicalPrincipal(principal) == "S-1-1-0" }) {
		t.Fatalf("child ACL %v misses the planted Everyone entry", polluted)
	}
	if err := auth.CheckHomeDir(home); err != nil {
		t.Fatalf("CheckHomeDir home: %v", err)
	}
	if err := auth.ProtectHomeTree(home); err != nil {
		t.Fatalf("reapply: %v", err)
	}
	user := processUserSIDString(t)
	for _, principal := range childAllowPrincipals(t, child) {
		if !principalAllowed(principal, user) {
			t.Fatalf("child still grants access to %s", principal)
		}
	}
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
