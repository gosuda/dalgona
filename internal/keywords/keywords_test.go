package keywords_test

import (
	"math/rand/v2"
	"slices"
	"strings"
	"testing"
	"unicode"

	"github.com/gosuda/dalgona/internal/keywords"
)

func TestBuiltinsProductMetadata(t *testing.T) {
	t.Parallel()

	dal := keywords.Builtins("dal")
	wantDal := []string{"ultrathink", "decide", "simplify"}
	if len(dal) != len(wantDal) {
		t.Fatalf("Builtins(dal) returned %d keywords, want %d", len(dal), len(wantDal))
	}
	for i := range wantDal {
		if dal[i].Word != wantDal[i] {
			t.Fatalf("Builtins(dal)[%d] = %q, want %q", i, dal[i].Word, wantDal[i])
		}
	}

	all := keywords.Builtins("dalgona")
	want := []expectedBuiltin{
		{word: "ultrathink", hue: [2]int{0, 330}, effort: "max"},
		{word: "jevify", hue: [2]int{300, 420}, tools: []string{"eval"}, effort: "default"},
		{word: "workflowz", hue: [2]int{30, 150}, tools: []string{"eval", "task"}, effort: "default"},
		{word: "orchestrate", hue: [2]int{150, 280}, tools: []string{"task"}, effort: "default"},
		{word: "ratchet", hue: [2]int{20, 70}, tools: []string{"ratchet", "exec", "task"}, effort: "default"},
		{word: "decide", hue: [2]int{200, 250}, effort: "default"},
		{word: "simplify", hue: [2]int{90, 140}, effort: "default"},
	}
	if len(all) != len(want) {
		t.Fatalf("Builtins(dalgona) returned %d keywords, want %d", len(all), len(want))
	}
	for i := range want {
		checkBuiltin(t, all[i], want[i])
	}
}

type expectedBuiltin struct {
	word   string
	hue    [2]int
	tools  []string
	effort string
}

func checkBuiltin(t *testing.T, got keywords.Keyword, want expectedBuiltin) {
	t.Helper()
	if got.Word != want.word || got.Hue != want.hue {
		t.Errorf("builtin identity = %q %v, want %q %v", got.Word, got.Hue, want.word, want.hue)
	}
	if !slices.Equal(got.Requires, want.tools) {
		t.Errorf("%s requirements = %v, want %v", got.Word, got.Requires, want.tools)
	}
	if got.Effort != want.effort {
		t.Errorf("%s effort = %q, want %q", got.Word, got.Effort, want.effort)
	}
	if got.Notice == "" || !got.Enabled {
		t.Errorf("%s must be enabled with an embedded notice", got.Word)
	}
}

func TestBuiltinsNotices(t *testing.T) {
	t.Parallel()

	all := keywords.Builtins("dalgona")
	noticeParts := map[string][]string{
		"ultrathink":  {"step by step"},
		"jevify":      {"ctx.judge_batch", "rubric", "journal"},
		"workflowz":   {"eval", "ctx.spawn", "ctx.scope", "ctx.wait", "journal"},
		"orchestrate": {"task", "yield", "Verify"},
		"ratchet":     {"ratchet", "approve", "gate", "exec", "task", "held-out"},
		"decide":      {"option", "Recommend", "change your recommendation"},
		"simplify":    {"Delete", "validation", "error path", "evidence"},
	}
	for _, keyword := range all {
		for _, part := range noticeParts[keyword.Word] {
			if !strings.Contains(keyword.Notice, part) {
				t.Errorf("%s notice does not contain %q", keyword.Word, part)
			}
		}
		for _, donorOnly := range []string{"workpool", "agent()", "{{", "}}", "sonic"} {
			if strings.Contains(keyword.Notice, donorOnly) {
				t.Errorf("%s notice retains donor-only text %q", keyword.Word, donorOnly)
			}
		}
	}
}

func TestMergeOverridesAndAppends(t *testing.T) {
	t.Parallel()

	builtins := []keywords.Keyword{
		{Word: "ultrathink", Hue: [2]int{0, 330}, Requires: []string{"read"}, Effort: "max", Notice: "think", Enabled: true},
		{Word: "jevify", Hue: [2]int{300, 420}, Requires: []string{"eval"}, Notice: "judge", Enabled: true},
	}
	custom := []keywords.Keyword{
		{Word: "ultrathink", Enabled: true},
		{Word: "jevify", Enabled: false},
		{Word: "review", Hue: [2]int{200, 260}, Notice: "inspect", Enabled: true},
	}
	got := keywords.Merge(builtins, custom)
	if gotWords := keywordWords(got); !slices.Equal(gotWords, []string{"ultrathink", "review"}) {
		t.Fatalf("Merge words = %v, want [ultrathink review]", gotWords)
	}
	if got[0].Hue != builtins[0].Hue || got[0].Effort != "max" || got[0].Notice != "think" || !slices.Equal(got[0].Requires, []string{"read"}) {
		t.Errorf("partial override did not preserve built-in fields: %#v", got[0])
	}
	if got[1].Notice != "inspect" || got[1].Hue != ([2]int{200, 260}) {
		t.Errorf("new custom keyword changed: %#v", got[1])
	}
}

func TestMergeClearsRequirements(t *testing.T) {
	t.Parallel()

	builtins := []keywords.Keyword{{Word: "ultrathink", Requires: []string{"read"}, Enabled: true}}
	cleared := keywords.Merge(builtins, []keywords.Keyword{{Word: "ultrathink", Requires: []string{}, Enabled: true}})
	if len(cleared) != 1 || len(cleared[0].Requires) != 0 {
		t.Errorf("explicit empty requirements did not replace inherited requirements: %#v", cleared)
	}
}

func TestMatchBoundariesAndMasks(t *testing.T) {
	t.Parallel()

	table := []keywords.Keyword{
		{Word: "ultrathink", Enabled: true},
		{Word: "jevify", Requires: []string{"eval"}, Enabled: true},
	}
	cases := []struct {
		name   string
		prompt string
		tools  []string
		want   []string
	}{
		{name: "comma", prompt: "ultrathink,", want: []string{"ultrathink"}},
		{name: "inline code", prompt: "`ultrathink`"},
		{name: "double inline code", prompt: "``ultrathink``"},
		{name: "fenced code", prompt: "```\nultrathink\n```"},
		{name: "tilde fence", prompt: "~~~text\nultrathink\n~~~"},
		{name: "indented fence", prompt: "   ```go\nultrathink\n   ```"},
		{name: "long closing fence", prompt: "````\n```\nultrathink\n```\n````"},
		{name: "unclosed fence", prompt: "```\nultrathink"},
		{name: "unmatched inline delimiter", prompt: "`broken\nultrathink", want: []string{"ultrathink"}},
		{name: "XML section", prompt: "<b>ultrathink</b>"},
		{name: "nested XML section", prompt: "<b>one <b>ultrathink</b> two</b>"},
		{name: "XML quoted bracket", prompt: "<b title='1 > 0'>ultrathink</b>"},
		{name: "XML comment", prompt: "<!-- ultrathink -->"},
		{name: "unclosed XML comment", prompt: "<!-- ultrathink"},
		{name: "self closing XML", prompt: "<b/> ultrathink", want: []string{"ultrathink"}},
		{name: "unclosed XML section", prompt: "<b> ultrathink", want: []string{"ultrathink"}},
		{name: "after XML section", prompt: "<b>hidden</b> ultrathink", want: []string{"ultrathink"}},
		{name: "less than expression", prompt: "1 < 2 && ultrathink", want: []string{"ultrathink"}},
		{name: "suffix letters", prompt: "ultrathinking"},
		{name: "left period", prompt: "x.ultrathink"},
		{name: "right call", prompt: "ultrathink("},
		{name: "uppercase", prompt: "ULTRATHINK"},
		{name: "left Unicode letter", prompt: "éultrathink"},
		{name: "right Unicode letter", prompt: "ultrathink界"},
		{name: "left Unicode digit", prompt: "١ultrathink"},
		{name: "double colon", prompt: "x::ultrathink"},
		{name: "single colon", prompt: "x:ultrathink", want: []string{"ultrathink"}},
		{name: "period letter", prompt: "ultrathink.a"},
		{name: "period digit", prompt: "ultrathink.1"},
		{name: "period underscore", prompt: "ultrathink._"},
		{name: "period hyphen", prompt: "ultrathink.-"},
		{name: "terminal period", prompt: "ultrathink.", want: []string{"ultrathink"}},
		{name: "two periods", prompt: "ultrathink..", want: []string{"ultrathink"}},
		{name: "missing requirement", prompt: "jevify"},
		{name: "met requirement", prompt: "jevify", tools: []string{"eval"}, want: []string{"jevify"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := keywordWords(keywords.Match(tc.prompt, table, tc.tools))
			if !slices.Equal(got, tc.want) {
				t.Fatalf("Match(%q) = %v, want %v", tc.prompt, got, tc.want)
			}
		})
	}
}

func TestMatchEveryForbiddenBoundary(t *testing.T) {
	t.Parallel()

	table := []keywords.Keyword{{Word: "ultrathink", Enabled: true}}
	for _, r := range []rune{'a', '1', '_', '.', '/', '\\', '-'} {
		if got := keywords.Match(string(r)+"ultrathink", table, nil); len(got) != 0 {
			t.Errorf("keyword after %q matched", r)
		}
	}
	for _, r := range []rune{'a', '1', '_', '/', '\\', '-', '('} {
		if got := keywords.Match("ultrathink"+string(r), table, nil); len(got) != 0 {
			t.Errorf("keyword before %q matched", r)
		}
	}
}

func TestMatchReturnsTableOrderOnce(t *testing.T) {
	t.Parallel()

	table := []keywords.Keyword{
		{Word: "orchestrate", Requires: []string{"task"}, Enabled: true},
		{Word: "ultrathink", Enabled: true},
		{Word: "workflowz", Requires: []string{"eval", "task"}, Enabled: true},
	}
	prompt := "ultrathink orchestrate workflowz ultrathink orchestrate workflowz"
	got := keywordWords(keywords.Match(prompt, table, []string{"eval", "task"}))
	want := []string{"orchestrate", "ultrathink", "workflowz"}
	if !slices.Equal(got, want) {
		t.Fatalf("Match order and duplicates = %v, want %v", got, want)
	}
}

func TestMatchAgainstUnicodeReference(t *testing.T) {
	t.Parallel()

	rng := rand.New(rand.NewPCG(41, 97))
	table := []keywords.Keyword{{Word: "ultrathink", Enabled: true}}
	alphabet := []rune(" abcdefghijklmnopqrstuvwxyz0123456789,.:/_\\-()!?é界١🙂")
	for range 2000 {
		prose := randomText(rng, alphabet, rng.IntN(25)) + "ultrathink" + randomText(rng, alphabet, rng.IntN(25))
		prompt := prose
		for range 4 {
			prompt = randomMaskedSegment(rng) + "\n" + prompt
		}
		want := referenceMatch(prose, "ultrathink")
		got := len(keywords.Match(prompt, table, nil)) == 1
		if got != want {
			t.Fatalf("reference mismatch for %q: got %v want %v", prompt, got, want)
		}
	}
}

func randomMaskedSegment(rng *rand.Rand) string {
	content := "ultrathink" + randomText(rng, []rune(" abc<~é"), rng.IntN(12))
	switch rng.IntN(6) {
	case 0:
		return "`" + content + "`"
	case 1:
		return "``" + content + "``"
	case 2:
		return "```text\n" + content + "\n```"
	case 3:
		return "~~~\n" + content + "\n~~~"
	case 4:
		return "<section>" + content + "</section>"
	default:
		return "<!-- " + content + " -->"
	}
}

func keywordWords(table []keywords.Keyword) []string {
	words := make([]string, len(table))
	for i, keyword := range table {
		words[i] = keyword.Word
	}
	return words
}

func randomText(rng *rand.Rand, alphabet []rune, n int) string {
	var b strings.Builder
	for range n {
		b.WriteRune(alphabet[rng.IntN(len(alphabet))])
	}
	return b.String()
}

type referenceKind uint8

const (
	referenceSeparator referenceKind = iota
	referenceWord
	referencePunctuation
)

type referenceToken struct {
	kind referenceKind
	text []rune
}

func referenceMatch(prompt, word string) bool {
	tokens := referenceTokens(prompt)
	for i, token := range tokens {
		if token.kind == referenceWord && string(token.text) == word && referenceLeft(tokens[i-1]) && referenceRight(tokens, i) {
			return true
		}
	}
	return false
}

func referenceTokens(prompt string) []referenceToken {
	tokens := []referenceToken{{kind: referenceSeparator}}
	for _, r := range prompt {
		kind := referenceRuneKind(r)
		last := len(tokens) - 1
		if tokens[last].kind == kind {
			tokens[last].text = append(tokens[last].text, r)
			continue
		}
		tokens = append(tokens, referenceToken{kind: kind, text: []rune{r}})
	}
	return append(tokens, referenceToken{kind: referenceSeparator})
}

func referenceRuneKind(r rune) referenceKind {
	if unicode.IsLetter(r) || unicode.IsDigit(r) {
		return referenceWord
	}
	if strings.ContainsRune("_./\\-():", r) {
		return referencePunctuation
	}
	return referenceSeparator
}

func referenceLeft(previous referenceToken) bool {
	return previous.kind == referenceSeparator || previous.kind == referencePunctuation && referenceLeftPunctuation(previous.text)
}

func referenceLeftPunctuation(text []rune) bool {
	last := text[len(text)-1]
	return !strings.ContainsRune("_./\\-", last) && !strings.HasSuffix(string(text), "::")
}

func referenceRight(tokens []referenceToken, wordIndex int) bool {
	next := tokens[wordIndex+1]
	if next.kind == referenceSeparator {
		return true
	}
	return next.kind == referencePunctuation && referenceRightPunctuation(tokens, wordIndex)
}

func referenceRightPunctuation(tokens []referenceToken, wordIndex int) bool {
	text := tokens[wordIndex+1].text
	if strings.ContainsRune("_/\\-(", text[0]) {
		return false
	}
	if text[0] != '.' {
		return true
	}
	if len(text) > 1 {
		return referencePeriodFollower(text[1])
	}
	return tokens[wordIndex+2].kind != referenceWord
}

func referencePeriodFollower(r rune) bool {
	return !unicode.IsLetter(r) && !unicode.IsDigit(r) && r != '_' && r != '-'
}
