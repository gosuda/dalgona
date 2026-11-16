package webfetch

import (
	"bytes"
	"errors"
	"fmt"
	"math/rand/v2"
	"net/url"
	"strings"
	"testing"
	"unicode/utf8"

	"golang.org/x/net/html"
)

func articleURL(t *testing.T, raw string) *url.URL {
	t.Helper()
	u, err := url.Parse(raw)
	if err != nil {
		t.Fatal(err)
	}
	return u
}

func TestHTMLToMarkdownMatchesGenerator(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(17, 29))
	original := articleURL(t, "https://original.example/start")
	final := articleURL(t, "https://final.example/news/article")
	for i := range 32 {
		word := fmt.Sprintf("Evidence%d", rng.Uint64())
		page := shellPage("<title>"+word+"</title>", `<nav>Navigation distractor</nav><article><h1>`+word+`</h1><p>`+strings.Repeat("The researchers measured ordinary events and recorded independent observations. ", 8)+`</p><h2>Findings</h2><ul><li>First `+word+`</li><li>Second finding</li></ul><p><a href="../details">Read evidence</a> <a href="javascript:alert(1)">Unsafe reference</a></p><pre><code>value := 42</code></pre><table><tr><th>Sample</th><th>Count</th></tr><tr><td>`+word+`</td><td>42</td></tr></table><p><img src="/plot.png" alt="Measured plot"></p></article><footer>Footer distractor</footer>`)
		got, err := renderBody([]byte(page), "text/html", original, final, defaultMaxBytes)
		if err != nil {
			t.Fatalf("case %d: %v", i, err)
		}
		text := string(got)
		for _, required := range []string{"# " + word, "## Findings", "- First " + word, "Second finding", "[Read evidence](https://final.example/details)", "value := 42", "Sample", "Count", "42", "https://final.example/plot.png"} {
			if !strings.Contains(text, required) {
				t.Fatalf("case %d missing %q in %q", i, required, text)
			}
		}
		for _, forbidden := range []string{"javascript:", "original.example", "<article", "<table", "Navigation distractor", "Footer distractor"} {
			if strings.Contains(text, forbidden) {
				t.Fatalf("case %d retained %q in %q", i, forbidden, text)
			}
		}
	}
}

func TestFetchTruncationKeepsUTF8(t *testing.T) {
	t.Parallel()
	page := []byte(shellPage("<title>한국어 기사</title>", "<article><h1>한국어 기사</h1><p>"+strings.Repeat("독립적으로 작성한 문장은 실제 기사의 내용을 보존합니다. ", 16)+"</p></article>"))
	u := articleURL(t, "https://example.com/article")
	full, err := renderBody(page, "text/html", u, u, defaultMaxBytes)
	if err != nil {
		t.Fatal(err)
	}
	for limit := int64(1); limit < int64(len(full)); limit++ {
		got := limitText(full, int64(len(full)), limit)
		notice := fmt.Sprintf("\n[content truncated at %d bytes]", limit)
		if !utf8.Valid(got) || !bytes.HasSuffix(got, []byte(notice)) {
			t.Fatalf("limit %d: invalid UTF-8 or missing notice: %q", limit, got)
		}
		if len(got)-len(notice) > int(limit) {
			t.Fatalf("limit %d: content exceeds limit", limit)
		}
	}
}

func TestFetchRenderingKinds(t *testing.T) {
	t.Parallel()
	u := articleURL(t, "https://example.com/article")
	for _, kind := range []string{"text/plain", "application/json", "application/xml"} {
		body := []byte("A & B <literal> 한국어")
		got, err := renderBody(body, kind, u, u, defaultMaxBytes)
		if err != nil || !bytes.Equal(got, body) {
			t.Fatalf("%s: %q, %v", kind, got, err)
		}
	}
	got, err := renderBody([]byte{0, 1, 2}, "image/png", u, u, defaultMaxBytes)
	if err != nil || string(got) != "[binary content image/png, 3 bytes]" {
		t.Fatalf("binary: %q, %v", got, err)
	}
	_, err = renderBody([]byte(shellPage(`<script src="app.js"></script>`, `<div id="app"></div>`)), "text/html", u, u, defaultMaxBytes)
	browser, ok := errors.AsType[*browserError](err)
	if !ok || browser.Error() != "web_fetch: page needs a browser: https://example.com/article" || renderFailure(u.String(), err).OK {
		t.Fatalf("shell: %v", err)
	}
}

func requireSanitizedLinks(t *testing.T, text string) {
	t.Helper()
	for _, forbidden := range []string{"mailto:", "%zz", "data:image", "src="} {
		if strings.Contains(text, forbidden) {
			t.Fatalf("unsafe reference remains: %s", text)
		}
	}
	if !strings.Contains(text, `href="https://example.com/safe"`) || !strings.Contains(text, "Mail text") || !strings.Contains(text, "Malformed text") {
		t.Fatalf("safe URL or link text lost: %s", text)
	}
}

func TestArticleTitleAndLinkSafety(t *testing.T) {
	t.Parallel()
	u := articleURL(t, "https://example.com/news/story")
	doc, err := html.Parse(strings.NewReader(`<article><h2>Existing title</h2><p><a href="/safe">Safe</a><a href="mailto:a@example.com">Mail text</a><a href="%zz">Malformed text</a><img src="data:image/png;base64,AA" alt="bad"></p></article>`))
	if err != nil {
		t.Fatal(err)
	}
	sanitizeLinks(doc, u)
	var serialized bytes.Buffer
	if err := html.Render(&serialized, doc); err != nil {
		t.Fatal(err)
	}
	requireSanitizedLinks(t, serialized.String())
	body := []byte("## Existing title")
	if got := withTitle(body, doc, "Existing\n title"); !bytes.Equal(got, body) {
		t.Fatalf("duplicate title: %q", got)
	}
	if got := string(withTitle(body, doc, "[New]*\n title")); !strings.HasPrefix(got, "# \\[New\\]\\* title\n\n") {
		t.Fatalf("unescaped title: %q", got)
	}
}
