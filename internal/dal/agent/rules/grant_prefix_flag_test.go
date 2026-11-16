package rules

import "testing"

func TestGrantPrefixKeepsFlagSecondToken(t *testing.T) {
	t.Parallel()
	prefix, ok := GrantPrefix("git -C x status")
	if !ok || prefix != "git -C" {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want %q", "git -C x status", prefix, ok, "git -C")
	}
	if MatchesGrant("git push", prefix) {
		t.Fatalf("MatchesGrant(%q, %q) = true; a status grant must not cover push", "git push", prefix)
	}
	if !MatchesGrant("git -C y status", prefix) {
		t.Fatalf("MatchesGrant(%q, %q) = false; same prefix must match", "git -C y status", prefix)
	}
}
