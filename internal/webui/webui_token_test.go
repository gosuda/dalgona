package webui

import (
	"strings"
	"testing"
)

func TestResolveTokenRejectsFragmentUnsafeTokens(t *testing.T) {
	for _, token := range []string{"abc+def", "a&b", "a b", "a%b", "a#b", "ü"} {
		_, err := resolveToken(Config{Token: token}, false)
		if err == nil {
			t.Fatalf("token %q accepted", token)
		}
		want := "web: [web] token must contain only [A-Za-z0-9_-]"
		if !strings.Contains(err.Error(), want) {
			t.Fatalf("token %q error = %v, want it to contain %q", token, err, want)
		}
	}
	long := strings.Repeat("k", 31) + "+"
	if _, err := resolveToken(Config{Token: long}, true); err == nil {
		t.Fatal("LAN token with '+' accepted")
	}
	for _, token := range []string{"abc", "abc-DEF_123"} {
		got, err := resolveToken(Config{Token: token}, false)
		if err != nil || got != token {
			t.Fatalf("token %q: got %q, %v", token, got, err)
		}
	}
}
