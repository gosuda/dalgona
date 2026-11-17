package webfetch

import (
	"strings"
	"testing"

	"golang.org/x/net/html"
)

func shellPage(head, body string) string {
	return "<!doctype html><html><head>" + head + "</head><body>" + body + "</body></html>"
}

func paragraph(text string) string { return "<p>" + text + "</p>" }

func TestAppShellBoundary(t *testing.T) {
	t.Parallel()
	const appScript = `<script src="/app.js"></script>`
	const dataScript = `<script type="application/ld+json">{"@type":"Article"}</script>`
	cases := []struct {
		name string
		page string
		want bool
	}{
		{"script and empty div", shellPage(appScript, `<div id="root"></div>`), true},
		{"script and 199 characters", shellPage(appScript, paragraph(strings.Repeat("a", 199))), true},
		{"script and 200 characters", shellPage(appScript, paragraph(strings.Repeat("a", 200))), false},
		{"data script and 50 characters", shellPage(dataScript, paragraph(strings.Repeat("a", 50))), false},
		{"data script and empty body", shellPage(dataScript, `<div id="root"></div>`), false},
		{"noscript text is ignored", shellPage(appScript, "<noscript>"+strings.Repeat("a", 300)+"</noscript>"+paragraph(strings.Repeat("b", 10))), true},
		{"same text outside noscript", shellPage(appScript, paragraph(strings.Repeat("a", 310))), false},
		{"zero scripts and 10 characters", shellPage("", paragraph(strings.Repeat("a", 10))), false},
		{"zero scripts and empty body", shellPage("", ""), false},
		{"module script", shellPage(`<script type="module" src="/m.js"></script>`, ""), true},
		{"text javascript script", shellPage(`<script type="text/javascript" src="/m.js"></script>`, ""), true},
		{"application javascript script", shellPage(`<script type="application/javascript" src="/m.js"></script>`, ""), true},
		{"javascript with parameters", shellPage(`<script type="Text/JavaScript; charset=utf-8" src="/m.js"></script>`, ""), true},
		{"script in body", shellPage("", `<div id="root"></div><script src="/m.js"></script>`), true},
		{"import map is data", shellPage(`<script type="importmap">{"imports":{}}</script>`, ""), false},
		{"template script is data", shellPage(`<script type="text/template">x</script>`, ""), false},
		{"json script is data", shellPage(`<script type="application/json">{}</script>`, ""), false},
		{"script source text is not visible", shellPage("<script>var a='"+strings.Repeat("a", 300)+"';</script>", ""), true},
		{"style text is not visible", shellPage(appScript, "<style>"+strings.Repeat("a", 300)+"</style>"), true},
		{"head title is not body text", shellPage("<title>"+strings.Repeat("a", 300)+"</title>"+appScript, paragraph(strings.Repeat("b", 10))), true},
		{"whitespace collapses", shellPage(appScript, paragraph("a"+strings.Repeat(" \n\t", 90)+"b")), true},
		{"separate words count their gap", shellPage(appScript, paragraph(strings.Repeat("ab ", 67))), false},
		{"199 Korean characters", shellPage(appScript, paragraph(strings.Repeat("가", 199))), true},
		{"200 Korean characters", shellPage(appScript, paragraph(strings.Repeat("가", 200))), false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			doc, err := html.Parse(strings.NewReader(tc.page))
			if err != nil {
				t.Fatal(err)
			}
			if got := LooksLikeAppShell(doc); got != tc.want {
				t.Fatalf("LooksLikeAppShell = %v, want %v", got, tc.want)
			}
		})
	}
}

func TestAppShellNilRoot(t *testing.T) {
	t.Parallel()
	if LooksLikeAppShell(nil) {
		t.Fatal("nil root reported as an app shell")
	}
}
