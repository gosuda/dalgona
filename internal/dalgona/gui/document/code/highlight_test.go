package code

import (
	"strings"
	"testing"
)

func tileCheck(t *testing.T, source string, spans []Span) {
	t.Helper()
	if len(spans) == 0 && len(source) > 0 {
		t.Fatalf("no spans for %d-byte source", len(source))
	}
	var rebuilt strings.Builder
	want := 0
	for i, s := range spans {
		if s.Start != want {
			t.Fatalf("span %d starts at %d, want %d", i, s.Start, want)
		}
		if s.End < s.Start {
			t.Fatalf("span %d ends before it starts", i)
		}
		want = s.End
		rebuilt.WriteString(source[s.Start:s.End])
	}
	if want != len(source) {
		t.Fatalf("spans cover [0,%d), source is %d bytes", want, len(source))
	}
	if rebuilt.String() != source {
		t.Fatal("concatenated spans do not reproduce the source exactly")
	}
}

func TestHighlightTilesSource(t *testing.T) {
	samples := map[string]string{
		"go":         "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"héllo, 世界 🌍\") // comment\n}\n",
		"python":     "def greet(name):\n    # 한글 주석\n    return f\"hello {name}!\"\n",
		"rust":       "fn main() {\n    let x: i32 = 0x2A; // comment\n    println!(\"{x}\");\n}\n",
		"typescript": "const s: string = `tpl ${1 + 2}`;\n// コメント\n",
		"javascript": "function f(a) { return a ?? 42; }\n",
		"json":       "{\"key\": [1, 2.5, true, null], \"uni\": \"\\u00e9\"}\n",
		"markdown":   "# Title\n\nSome **bold** and `code`.\n",
		"bash":       "#!/bin/sh\n# 주석\necho \"hi $USER\" && ls | grep x\n",
		"css":        "body {\n  color: #fff; /* comment */\n}\n",
		"sql":        "SELECT id, name FROM users WHERE id = 1; -- comment\n",
		"xml":        "<?xml version=\"1.0\"?>\n<root attr=\"v\">text</root>\n",
		"yaml":       "key: value # comment\nlist:\n  - 1\n  - two\n",
		"ini":        "[section]\nkey=value ; comment\n",
		"diff":       "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n",
		"dockerfile": "FROM golang:1.27 AS build # comment\nRUN go build ./...\n",
	}
	for lang, src := range samples {
		b, err := Highlight(src, lang)
		if err != nil {
			t.Fatalf("%s: Highlight error: %v", lang, err)
		}
		if !b.Known {
			t.Fatalf("%s: expected known language", lang)
		}
		tileCheck(t, src, b.Spans)
	}
}

func TestHighlightAliases(t *testing.T) {
	for _, alias := range []string{"sh", "shell", "toml", "html", "ts", "js"} {
		b, err := Highlight("x", alias)
		if err != nil {
			t.Fatalf("%s: Highlight error: %v", alias, err)
		}
		if !b.Known {
			t.Fatalf("alias %s did not resolve to a known language", alias)
		}
	}
}

func TestHighlightUnknownLanguagePlain(t *testing.T) {
	src := "some <text> & bytes\nline two\n"
	b, err := Highlight(src, "klingon")
	if err != nil {
		t.Fatalf("unknown language must not error: %v", err)
	}
	if b.Known {
		t.Fatal("unknown language must not report known")
	}
	if b.Label != "klingon" {
		t.Fatalf("language label not preserved, got %q", b.Label)
	}
	tileCheck(t, src, b.Spans)
	for _, s := range b.Spans {
		if s.Kind != PlainText {
			t.Fatalf("plain fallback span has kind %v", s.Kind)
		}
	}
}

func TestHighlightRejectsInvalidUTF8(t *testing.T) {
	_, err := Highlight("valid prefix \xff\xfe broken", "go")
	if err == nil {
		t.Fatal("invalid UTF-8 source must be refused")
	}
}

func TestHighlightBudgetLeavesPlainCode(t *testing.T) {
	src := strings.Repeat("x = 1\n", (MaxHighlightBytes/6)+1)
	b, err := Highlight(src, "python")
	if err == nil {
		t.Fatal("oversized source must report the highlighting budget")
	}
	tileCheck(t, src, b.Spans)
}
