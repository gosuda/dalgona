package rpc

import (
	"testing"
)

func TestAuditPlatformOsClampedToSpec(t *testing.T) {
	for _, goos := range []string{"freebsd", "openbsd", "netbsd", "solaris"} {
		if got := platformOSType(goos); got != "linux" {
			t.Fatalf("platformOSType(%q)=%q, want linux", goos, got)
		}
	}
	if got := platformOSType("darwin"); got != "macos" {
		t.Fatalf("darwin=%q, want macos", got)
	}
	if got := platformOSType("windows"); got != "windows" {
		t.Fatalf("windows=%q, want windows", got)
	}
	if got := platformOSType("linux"); got != "linux" {
		t.Fatalf("linux=%q, want linux", got)
	}
}
