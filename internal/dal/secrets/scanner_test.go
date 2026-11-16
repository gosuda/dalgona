package secrets_test

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

func pemBlock(body string) string {
	return "-----BEGIN " + "RSA PRIVATE KEY-----\n" + body + "\n-----END " + "RSA PRIVATE KEY-----"
}

func jwtOf(n int) string {
	return "eyJ" + strings.Repeat("h", n) + "." + strings.Repeat("p", n) + "." + strings.Repeat("s", n)
}

type shape struct {
	rule   int
	typ    string
	secret string
}

func shapes() []shape {
	return []shape{
		{0, "aws_access_key_id", "AKIA" + strings.Repeat("Q", 16)},
		{1, "github_token", "ghp_" + strings.Repeat("a", 36)},
		{2, "github_pat", "github_pat_" + strings.Repeat("b", 22)},
		{3, "slack_token", "xoxb-" + strings.Repeat("1", 10)},
		{4, "google_api_key", "AIza" + strings.Repeat("c", 35)},
		{5, "anthropic_key", "sk-ant-api03-" + strings.Repeat("d", 20)},
		{6, "openai_key", "sk-" + strings.Repeat("e", 20)},
		{6, "openai_key", "sk-proj-" + strings.Repeat("f", 24)},
		{7, "private_key_block", pemBlock(strings.Repeat("k", 24))},
		{8, "jwt", jwtOf(16)},
	}
}

func TestBuiltinSecretPatterns(t *testing.T) {
	for _, item := range shapes() {
		text := "log line before " + item.secret + " and after"
		hits := secrets.Scan(text)
		if len(hits) != 1 {
			t.Fatalf("%s: got %d hits, want 1: %+v", item.typ, len(hits), hits)
		}
		hit := hits[0]
		if hit.Type != item.typ || hit.Rule != item.rule {
			t.Fatalf("%s: got type %q rule %d, want rule %d", item.typ, hit.Type, hit.Rule, item.rule)
		}
		if got := text[hit.Start:hit.End]; got != item.secret {
			t.Fatalf("%s: span = %q, want %q", item.typ, got, item.secret)
		}
	}
}

func TestScanLengthBoundaries(t *testing.T) {
	cases := []struct {
		name string
		text string
		want int
	}{
		{"aws at 16", "AKIA" + strings.Repeat("Q", 16), 1},
		{"aws at 15", "AKIA" + strings.Repeat("Q", 15), 0},
		{"openai at 20", "sk-" + strings.Repeat("e", 20), 1},
		{"openai at 19", "sk-" + strings.Repeat("e", 19), 0},
		{"github token at 36", "ghp_" + strings.Repeat("a", 36), 1},
		{"github token at 35", "ghp_" + strings.Repeat("a", 35), 0},
		{"github pat at 22", "github_pat_" + strings.Repeat("b", 22), 1},
		{"github pat at 21", "github_pat_" + strings.Repeat("b", 21), 0},
		{"slack at 10", "xoxb-" + strings.Repeat("1", 10), 1},
		{"slack at 9", "xoxb-" + strings.Repeat("1", 9), 0},
		{"google at 35", "AIza" + strings.Repeat("c", 35), 1},
		{"google at 34", "AIza" + strings.Repeat("c", 34), 0},
		{"anthropic at 20", "sk-ant-" + strings.Repeat("d", 20), 1},
		{"anthropic at 19", "sk-ant-" + strings.Repeat("d", 19), 0},
		{"jwt at 10", jwtOf(10), 1},
		{"jwt at 9", jwtOf(9), 0},
		{"two segments", "eyJ" + strings.Repeat("h", 16) + "." + strings.Repeat("p", 16), 0},
		{"dotted words", "a.b.c", 0},
		{"unterminated private key", "-----BEGIN " + "PRIVATE KEY-----\nbody", 0},
	}
	for _, item := range cases {
		if got := len(secrets.Scan(item.text)); got != item.want {
			t.Errorf("%s: %d hits, want %d", item.name, got, item.want)
		}
	}
}

func TestScanKebabGuard(t *testing.T) {
	cases := []struct {
		name string
		text string
		want int
	}{
		{"legacy alphanumeric key", "sk-" + strings.Repeat("e", 24), 1},
		{"known project prefix", "sk-proj-" + strings.Repeat("f", 24), 1},
		{"known service account prefix", "sk-svcacct-" + strings.Repeat("f", 24), 1},
		{"known admin prefix", "sk-admin-" + strings.Repeat("f", 24), 1},
		{"vendor prefix with a digit", "sk-fw-1" + strings.Repeat("a", 20), 1},
		{"router prefix with a digit", "sk-or-v1-" + strings.Repeat("a", 24), 1},
		{"vendor prefix without a digit", "sk-fw-" + strings.Repeat("a", 24), 0},
		{"short kebab phrase", "sk-learn-library", 0},
		{"long kebab phrase", "sk-machine-learning-library", 0},
		{"kebab phrase inside a word", "task-management-and-coordination-plan", 0},
	}
	for _, item := range cases {
		if got := len(secrets.Scan(item.text)); got != item.want {
			t.Errorf("%s: %d hits, want %d", item.name, got, item.want)
		}
	}
}

func hitTypes(hits []secrets.Hit) []string {
	types := make([]string, 0, len(hits))
	for _, hit := range hits {
		types = append(types, hit.Type)
	}
	return types
}

func TestScanOrdersByStart(t *testing.T) {
	text := "first " + shapes()[1].secret + " then " + shapes()[0].secret
	hits := secrets.Scan(text)
	if got := hitTypes(hits); !slices.Equal(got, []string{"github_token", "aws_access_key_id"}) {
		t.Fatalf("types = %v", got)
	}
	if hits[0].Start >= hits[1].Start {
		t.Fatalf("starts = %d, %d", hits[0].Start, hits[1].Start)
	}
}

func TestScanKeepsOnlyTheOutermostBlock(t *testing.T) {
	nested := pemBlock("body " + shapes()[0].secret + " tail")
	hits := secrets.Scan(nested)
	if got := hitTypes(hits); !slices.Equal(got, []string{"private_key_block"}) {
		t.Fatalf("types = %v", got)
	}
	if hits[0].Start != 0 || hits[0].End != len(nested) {
		t.Fatalf("span = %d..%d, want 0..%d", hits[0].Start, hits[0].End, len(nested))
	}
}

func TestScanEqualSpanPicksLowerRule(t *testing.T) {
	hits := secrets.Scan(shapes()[5].secret)
	if got := hitTypes(hits); !slices.Equal(got, []string{"anthropic_key"}) {
		t.Fatalf("types = %v", got)
	}
}

func TestScanReportsRepeatedValueTwice(t *testing.T) {
	aws := shapes()[0].secret
	hits := secrets.Scan(aws + " " + aws)
	if len(hits) != 2 || hits[1].Start != len(aws)+1 {
		t.Fatalf("hits = %+v", hits)
	}
}

func TestScanCoversSecretBeforeWordCharacters(t *testing.T) {
	aws := shapes()[0].secret
	for _, suffix := range []string{"_suffix", "EXTRA", "extra"} {
		text := aws + suffix
		hits := secrets.Scan(text)
		if len(hits) != 1 || text[hits[0].Start:hits[0].End] != aws {
			t.Fatalf("suffix %q: hits = %+v", suffix, hits)
		}
	}
}

func TestScanIgnoresOrdinaryText(t *testing.T) {
	for _, text := range []string{
		"",
		"the quick brown fox jumps over the lazy dog",
		`func main() { fmt.Println("hello") }`,
		"commit a1b2c3d4 fixed the build",
		"export PATH=/usr/local/bin:$PATH",
		"see task-management-and-coordination-plan for the roadmap",
		"{{secret:auth:openai}} {{secret:pattern0:0}}",
	} {
		if hits := secrets.Scan(text); len(hits) != 0 {
			t.Errorf("false positive on %q: %+v", text, hits)
		}
	}
}
