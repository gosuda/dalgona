package rules

import "testing"

func TestGrantPrefixRejectsUnterminatedQuote(t *testing.T) {
	t.Parallel()
	prefix, ok := GrantPrefix(`echo "abc`)
	if ok || prefix != "" {
		t.Fatalf("GrantPrefix(%q) = %q, %v; want \"\", false", `echo "abc`, prefix, ok)
	}
}
