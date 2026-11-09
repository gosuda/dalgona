package auth

import (
	"strings"
	"testing"
)

func TestStore_UnmaskStrictIgnoresNonGrammar(t *testing.T) {
	home := t.TempDir()
	store, err := NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	const literal = "prefix {{secret:example}} suffix"
	if got, err := store.UnmaskStrict(literal); err != nil || got != literal {
		t.Fatalf("UnmaskStrict(%q) = %q, %v, want unchanged", literal, got, err)
	}
	const unknown = "{{secret:unknown}}"
	if got, err := store.UnmaskStrict(unknown); err != nil || got != unknown {
		t.Fatalf("UnmaskStrict(%q) = %q, %v, want unchanged", unknown, got, err)
	}
}

func TestStore_UnmaskStrictFailsOnMissingGrammar(t *testing.T) {
	home := t.TempDir()
	store, err := NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	for _, token := range []string{
		"{{secret:pattern0:0}}",
		"{{secret:STRICT_SCOPE_TEST_API_KEY}}",
		"{{secret:auth:openai}}",
	} {
		if _, err := store.UnmaskStrict("prefix " + token + " suffix"); err == nil {
			t.Fatalf("UnmaskStrict(%q) succeeded, want error", token)
		} else if !strings.Contains(err.Error(), "cannot be restored") {
			t.Fatalf("UnmaskStrict(%q) error = %q, want restore error", token, err)
		}
	}
}
