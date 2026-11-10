package auth

import (
	"strings"
	"testing"
)

const (
	testHome = "C:\\Users\\case\\home"
	testUser = "S-1-5-21-100-200-300-1001"
)

func privateSDDL(user string) string {
	return "O:" + user + "G:" + user + "D:P(A;OICI;GA;;;" + user + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
}

func checkHomeSDDL(dir, sddl, user string) error {
	return checkHomeSDDLAllow(dir, sddl, stringAllow(user))
}

func stringAllow(user string) func(string) (bool, string) {
	return func(principal string) (bool, string) {
		switch principal {
		case user, "S-1-5-18", "SY", "S-1-5-32-544", "BA":
			return true, ""
		default:
			return false, canonicalTestSID(principal)
		}
	}
}

func canonicalTestSID(principal string) string {
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

func TestCheckHomeSDDLRefusesEveryone(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(A;OICI;GA;;;" + testUser + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)(A;OICI;GA;;;WD)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "S-1-1-0") {
		t.Fatalf("checkHomeSDDL = %v, want refusal naming S-1-1-0", err)
	}
}

func TestCheckHomeSDDLAcceptsPrivateHome(t *testing.T) {
	if err := checkHomeSDDL(testHome, privateSDDL(testUser), testUser); err != nil {
		t.Fatalf("checkHomeSDDL = %v", err)
	}
}

func TestCheckHomeSDDLRefusesInheritedDACL(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:AI(A;OICI;GA;;;" + testUser + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "inherits") {
		t.Fatalf("checkHomeSDDL = %v, want inheritance refusal", err)
	}
}

func TestCheckHomeSDDLRefusesNullDACL(t *testing.T) {
	for _, sddl := range []string{
		"O:" + testUser + "G:" + testUser,
		"O:" + testUser + "G:" + testUser + "D:NO_ACCESS_CONTROL",
	} {
		if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "everyone") {
			t.Fatalf("checkHomeSDDL(%q) = %v, want refusal granting everyone", sddl, err)
		}
	}
}

func TestCheckHomeSDDLIgnoresDenyEntries(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(D;OICI;GA;;;WD)(A;OICI;GA;;;" + testUser + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err != nil {
		t.Fatalf("checkHomeSDDL = %v", err)
	}
}

func TestCheckHomeSDDLRefusesEmptyDACL(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "empty") {
		t.Fatalf("checkHomeSDDL = %v, want empty refusal", err)
	}
}

func TestCheckHomeSDDLRefusesInheritOnlyEveryone(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(A;OICI;GA;;;" + testUser + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)(A;OICIIO;GA;;;WD)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "S-1-1-0") {
		t.Fatalf("checkHomeSDDL = %v, want refusal naming S-1-1-0", err)
	}
}

func TestCheckHomeSDDLRefusesObjectAllowEveryone(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(A;OICI;GA;;;" + testUser + ")(OA;OICI;GA;;;SY)(OA;OICI;GA;;;BA)(OA;OICI;GA;;;WD)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "S-1-1-0") {
		t.Fatalf("checkHomeSDDL = %v, want refusal naming S-1-1-0", err)
	}
}

func TestCheckHomeSDDLRefusesConditionalAllowEveryone(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(A;OICI;GA;;;" + testUser + ")(XA;OICI;FA;;;SY;(Member_of {SID(SY)}))(XA;OICI;FA;;;WD;(Member_of {SID(WD)}))"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "S-1-1-0") {
		t.Fatalf("checkHomeSDDL = %v, want refusal naming S-1-1-0", err)
	}
}

func TestCheckHomeSDDLIgnoresAuditAndDenyObjectEntries(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(AU;OICI;FA;;;WD)(OD;OICI;GA;;;WD)(XD;OICI;FA;;;WD;(Member_of {SID(WD)}))(A;OICI;GA;;;" + testUser + ")(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err != nil {
		t.Fatalf("checkHomeSDDL = %v", err)
	}
}

func TestCheckHomeSDDLRefusesUnknownType(t *testing.T) {
	sddl := "O:" + testUser + "G:" + testUser + "D:P(A;OICI;GA;;;" + testUser + ")(XX;OICI;GA;;;SY)"
	if err := checkHomeSDDL(testHome, sddl, testUser); err == nil || !strings.Contains(err.Error(), "unknown ACE type") {
		t.Fatalf("checkHomeSDDL = %v, want unknown type refusal", err)
	}
}

func TestCheckHomeSDDLAllowResolvesOwnerAlias(t *testing.T) {
	owner := "S-1-5-21-100-200-300-500"
	sddl := "O:" + owner + "G:" + owner + "D:P(A;OICI;GA;;;LA)(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)"
	if err := checkHomeSDDLAllow(testHome, sddl, ownerAliasAllow(owner, owner)); err != nil {
		t.Fatalf("checkHomeSDDLAllow with owner alias = %v", err)
	}
	if err := checkHomeSDDLAllow(testHome, sddl, ownerAliasAllow(owner, "S-1-5-21-999-999-999-500")); err == nil || !strings.Contains(err.Error(), "S-1-5-21-999-999-999-500") {
		t.Fatalf("checkHomeSDDLAllow with foreign alias = %v, want refusal naming resolved SID", err)
	}
}

func ownerAliasAllow(owner, aliasSID string) func(string) (bool, string) {
	return func(principal string) (bool, string) {
		if principal == "LA" {
			principal = aliasSID
		}
		switch principal {
		case owner, "S-1-5-18", "SY", "S-1-5-32-544", "BA":
			return true, ""
		default:
			return false, principal
		}
	}
}
