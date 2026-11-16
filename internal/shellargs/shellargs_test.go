package shellargs

import (
	"math/rand/v2"
	"slices"
	"strings"
	"testing"
)

func TestSplitProperty(t *testing.T) {
	rng := rand.New(rand.NewPCG(1, 2))
	safe := []string{"go test ./...", `printf "%s" "a&&b"`, `echo a\|b`, "git status", `printf 'x;y|z'`, `printf "a\"&&b"`, "echo a\\\nb", `printf '' "" x`}
	operators := []string{"&&", "||", ";", "|", "\n"}
	for range 500 {
		parts := make([]string, 1+rng.IntN(8))
		var command strings.Builder
		for i := range parts {
			if i > 0 {
				command.WriteString(" " + operators[rng.IntN(len(operators))] + " ")
			}
			parts[i] = safe[rng.IntN(len(safe))]
			command.WriteString(parts[i])
		}
		if got := Split(command.String()); !slices.Equal(got, parts) {
			t.Fatalf("Split(%q) = %q, want %q", command.String(), got, parts)
		}
	}
}

const operatorRunes = ";|&(){}\n"

func bareAlphabet() []rune {
	return []rune("abZ09$*?[]~é日😀;|&(){}")
}

func fullAlphabet() []rune {
	return append(bareAlphabet(), []rune(" \t\n'\"\\\u00a0\u3000")...)
}

func generatedTokens(count int) []string {
	rng := rand.New(rand.NewPCG(3, 4))
	tokens := make([]string, 1, count)
	for range count - 1 {
		alphabet := fullAlphabet()
		if rng.IntN(4) == 0 {
			alphabet = bareAlphabet()
		}
		runes := make([]rune, 1+rng.IntN(12))
		for j := range runes {
			runes[j] = alphabet[rng.IntN(len(alphabet))]
		}
		tokens = append(tokens, string(runes))
	}
	return tokens
}

func readsBackAsOneWord(token string, quote func(string) string) bool {
	words, ok := Tokens(quote(token))
	return ok && slices.Equal(words, []string{token})
}

func TestQuoteTokenRoundTripsThroughTokens(t *testing.T) {
	var quoted, unchanged int
	for _, token := range generatedTokens(2000) {
		if !readsBackAsOneWord(token, QuoteToken) {
			words, ok := Tokens(QuoteToken(token))
			t.Fatalf("Tokens(QuoteToken(%q)) = %q, %v, want [%q], true", token, words, ok, token)
		}
		if QuoteToken(token) == token {
			unchanged++
		} else {
			quoted++
		}
	}
	if quoted < 500 || unchanged < 100 {
		t.Fatalf("generated %d quoted and %d unchanged tokens, want at least 500 and 100", quoted, unchanged)
	}
}

func TestQuoteTokenKeepsOneSplitPart(t *testing.T) {
	for _, token := range generatedTokens(2000) {
		quoted := QuoteToken(token)
		if quoted == token && strings.ContainsAny(token, operatorRunes) {
			continue
		}
		if parts := Split(quoted); !slices.Equal(parts, []string{quoted}) {
			t.Fatalf("Split(QuoteToken(%q)) = %q, want [%q]", token, parts, quoted)
		}
	}
}

func TestRoundTripCheckRejectsBrokenQuoters(t *testing.T) {
	if words, ok := Tokens("a b"); !ok || !slices.Equal(words, []string{"a", "b"}) {
		t.Fatalf("Tokens(%q) = %q, %v, want [a b], true", "a b", words, ok)
	}
	if got := QuoteToken("a b"); got == "a b" {
		t.Fatalf("QuoteToken(%q) = %q, want a quoted form", "a b", got)
	}
	mutants := map[string]func(string) string{
		"identity": func(token string) string { return token },
		"empty token left bare": func(token string) string {
			if token == "" {
				return token
			}
			return QuoteToken(token)
		},
		"single quote left bare": func(token string) string {
			return "'" + token + "'"
		},
		"backslash left bare": func(token string) string {
			if strings.ContainsAny(token, " \t\n\u00a0\u3000'\"") {
				return QuoteToken(token)
			}
			return token
		},
	}
	tokens := generatedTokens(2000)
	for name, quote := range mutants {
		if !slices.ContainsFunc(tokens, func(token string) bool { return !readsBackAsOneWord(token, quote) }) {
			t.Fatalf("mutant %q read back every generated token as one word", name)
		}
	}
}

func TestQuoteToken(t *testing.T) {
	cases := []struct{ token, want string }{
		{"", `''`},
		{"ls", "ls"},
		{"a b", `'a b'`},
		{"a\tb", "'a\tb'"},
		{"a\nb", "'a\nb'"},
		{"it's", `'it'\''s'`},
		{"'", `''\'''`},
		{`a"b`, `'a"b'`},
		{`a\b`, `'a\b'`},
		{"$HOME", "$HOME"},
		{"*.go?[x]", "*.go?[x]"},
		{"é日😀", "é日😀"},
		{"日本 語", "'日本 語'"},
		{"a\u00a0b", "'a\u00a0b'"},
		{"a;b|c&d", "a;b|c&d"},
	}
	for _, c := range cases {
		if got := QuoteToken(c.token); got != c.want {
			t.Errorf("QuoteToken(%q) = %q, want %q", c.token, got, c.want)
		}
	}
}

func TestTokens(t *testing.T) {
	cases := []struct {
		name  string
		input string
		words []string
		ok    bool
	}{
		{"empty input", "", nil, true},
		{"only spaces", "   ", nil, true},
		{"only tabs and spaces", "\t \t ", nil, true},
		{"empty single quotes in the middle", `a '' b`, []string{"a", "", "b"}, true},
		{"empty single quotes alone", `''`, []string{""}, true},
		{"empty double quotes alone", `""`, []string{""}, true},
		{"empty double quotes at the end", `a ""`, []string{"a", ""}, true},
		{"two empty quoted words", `'' ""`, []string{"", ""}, true},
		{"empty quotes touching a word", `a''b ''c d""`, []string{"ab", "c", "d"}, true},
		{"empty quotes between quoted runs", `'a'''b`, []string{"ab"}, true},
		{"unterminated quote after empty quotes", `'' '`, nil, false},
		{"escaped quote makes a word and not an empty one", `\'`, []string{"'"}, true},
		{"spaces around words", "  go   test  ", []string{"go", "test"}, true},
		{"single quotes group", `'a b' c`, []string{"a b", "c"}, true},
		{"double quotes group", `"a b" c`, []string{"a b", "c"}, true},
		{"adjacent single quoted runs", `'a''b'`, []string{"ab"}, true},
		{"adjacent double quoted runs", `"a""b"`, []string{"ab"}, true},
		{"adjacent mixed quotes and bare text", `'a'"b"c`, []string{"abc"}, true},
		{"quoted run inside a bare word", `x'y z'w`, []string{"xy zw"}, true},
		{"escaped quote inside double quotes", `"a\"b"`, []string{`a"b`}, true},
		{"escaped backslash inside double quotes", `"a\\b"`, []string{`a\b`}, true},
		{"escape inside double quotes drops the backslash", `"a\nb"`, []string{"anb"}, true},
		{"backslash is plain inside single quotes", `'a\b'`, []string{`a\b`}, true},
		{"single quote closes after a backslash", `'a\'`, []string{`a\`}, true},
		{"escaped space outside quotes", `a\ b`, []string{"a b"}, true},
		{"escaped quotes outside quotes", `\"a\'`, []string{`"a'`}, true},
		{"escaped backslash at the end", `a\\`, []string{`a\`}, true},
		{"escaped newline", "a\\\nb", []string{"a\nb"}, true},
		{"operator characters stay as text", `a;b|c&d(e)f{g}h`, []string{`a;b|c&d(e)f{g}h`}, true},
		{"operator word between spaces", `a && b`, []string{"a", "&&", "b"}, true},
		{"newline stays as text", "a\nb", []string{"a\nb"}, true},
		{"operator inside quotes", `"a;b"`, []string{"a;b"}, true},
		{"no expansion of dollar and glob", `echo '$x' *?`, []string{"echo", "$x", "*?"}, true},
		{"non-ASCII text", `"日本 語" é`, []string{"日本 語", "é"}, true},
		{"unterminated single quote", `'abc`, nil, false},
		{"unterminated double quote", `"abc`, nil, false},
		{"unterminated quote after a word", `a "b c`, nil, false},
		{"double quote inside single quotes stays open", `'a"b`, nil, false},
		{"single quote inside double quotes stays open", `"a'b`, nil, false},
		{"escaped closing double quote leaves it open", `"a\"`, nil, false},
		{"closed double quote control", `"a\\"`, []string{`a\`}, true},
		{"trailing backslash", `abc\`, nil, false},
		{"only a backslash", `\`, nil, false},
		{"trailing backslash after a word", `a b\`, nil, false},
		{"trailing backslash inside double quotes", `"abc\`, nil, false},
		{"trailing backslash inside single quotes", `'abc\`, nil, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			words, ok := Tokens(c.input)
			if ok != c.ok || c.ok && !slices.Equal(words, c.words) {
				t.Fatalf("Tokens(%q) = %q, %v, want %q, %v", c.input, words, ok, c.words, c.ok)
			}
		})
	}
}
