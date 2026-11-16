package rules

import "testing"

func TestGrantPrefixRejectsUnterminatedQuote(t *testing.T) {
	t.Parallel()
	prefix, ok := GrantPrefix(`echo "abc`)
	if ok || prefix != "" {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want \"\", false", `echo "abc`, prefix, ok)
	}
}

func TestGrantPrefixRejectsTrailingBackslash(t *testing.T) {
	t.Parallel()
	prefix, ok := GrantPrefix(`echo abc\`)
	if ok || prefix != "" {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want \"\", false", `echo abc\`, prefix, ok)
	}
}

func TestGrantPrefixAcceptsWellFormedPrefix(t *testing.T) {
	t.Parallel()
	prefix, ok := GrantPrefix("echo abc")
	if !ok || prefix != "echo abc" {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want %q, true", "echo abc", prefix, ok, "echo abc")
	}
}
