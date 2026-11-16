package commentgate

import (
	"fmt"
	"go/scanner"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"
)

type finding struct {
	path string
	line int
	text string
}

type legalRecord struct {
	path   string
	notice string
}

var legalNotices []legalRecord

var goAllowed = regexp.MustCompile(`^//(go:(build|embed|generate) [^ ]|nolint:[a-z][a-z0-9,-]* // [^ ])`)

var skipAnywhere = map[string]bool{
	".git": true, ".references": true, ".outline": true, ".omp": true, "node_modules": true, ".svelte-kit": true,
}

var skipAtRoot = map[string]bool{
	"dalgona-private": true, "local": true, ".local": true, "target": true,
	".build-target": true, ".ruff_cache": true, "bin": true, "dist": true,
}

var skipPaths = []string{"internal/dal/tui/nomnomljs", "internal/webui/dist", "web/src/lib/codex"}

var scanned = map[string]bool{
	".go": true, ".ts": true, ".svelte": true, ".py": true,
	".js": true, ".mjs": true, ".cjs": true, ".jsx": true, ".tsx": true,
}

func scanGo(path string, src []byte, notices []legalRecord) []finding {
	fset := token.NewFileSet()
	file := fset.AddFile(path, -1, len(src))
	var s scanner.Scanner
	s.Init(file, src, nil, scanner.ScanComments)
	legal := legalLineSet(path, src, notices)
	var out []finding
	for {
		pos, tok, lit := s.Scan()
		if tok == token.EOF {
			return out
		}
		line := fset.Position(pos).Line
		if tok == token.COMMENT && !goAllowed.MatchString(lit) && !legalGoComment(path, lit, line, legal, notices) {
			out = append(out, finding{path, line, lit})
		}
	}
}

func legalGoComment(path, lit string, line int, legal map[int]bool, notices []legalRecord) bool {
	if strings.HasPrefix(lit, "//") && legal[line] {
		return true
	}
	for _, rec := range notices {
		if rec.path == path && rec.notice == lit {
			return true
		}
	}
	return false
}

func scanLines(path string, src []byte, notices []legalRecord) []finding {
	ext := filepath.Ext(path)
	legal := legalLineSet(path, src, notices)
	var out []finding
	zone := zoneMarkup
	for i, raw := range strings.Split(string(src), "\n") {
		line := i + 1
		text := strings.Trim(raw, " \t")
		if !legal[line] {
			if ext == ".svelte" {
				if findingText, ok := svelteProse(zone, text); ok {
					out = append(out, finding{path, line, findingText})
				}
			} else if findingText, ok := proseLine(ext, text); ok {
				out = append(out, finding{path, line, findingText})
			}
		}
		if ext == ".svelte" {
			zone = svelteZoneAfter(zone, raw)
		}
	}
	return out
}

func legalLineSet(path string, src []byte, notices []legalRecord) map[int]bool {
	var set map[int]bool
	for _, rec := range notices {
		if rec.path != path {
			continue
		}
		for _, span := range noticeSpans(src, rec.notice) {
			if set == nil {
				set = map[int]bool{}
			}
			for line := span[0]; line <= span[1]; line++ {
				set[line] = true
			}
		}
	}
	return set
}

func noticeSpans(src []byte, notice string) [][2]int {
	if notice == "" {
		return nil
	}
	srcLines := strings.Split(string(src), "\n")
	noticeLines := strings.Split(strings.TrimSuffix(notice, "\n"), "\n")
	var spans [][2]int
	for i := 0; i+len(noticeLines) <= len(srcLines); i++ {
		match := true
		for j, want := range noticeLines {
			if srcLines[i+j] != want {
				match = false
				break
			}
		}
		if match {
			spans = append(spans, [2]int{i + 1, i + len(noticeLines)})
		}
	}
	return spans
}

type proseRule func(text string) (string, bool)

var proseRules = map[string]proseRule{
	".ts": tsProse, ".js": tsProse, ".mjs": tsProse, ".cjs": tsProse, ".jsx": tsProse, ".tsx": tsProse,
	".py": pythonProse,
}

func proseLine(ext, text string) (string, bool) {
	rule, ok := proseRules[ext]
	if !ok {
		return "", false
	}
	return rule(text)
}

type svelteZone int

const (
	zoneMarkup svelteZone = iota
	zoneScript
	zoneStyle
)

var (
	tsMarkers         = []string{"//", "/*"}
	tsQuotes          = []string{"'", `"`, "`"}
	pythonMarkers     = []string{"#", `"""`}
	pythonQuotes      = []string{"'", `"`, `"""`}
	svelteMarkupMarks = []string{"<!--"}
	svelteExprMarks   = []string{"/*"}
	svelteScriptMarks = []string{"//", "/*", "<!--"}
	svelteStyleMarks  = []string{"/*", "<!--"}
	svelteQuotes      = []string{"'", `"`}
)

func tsProse(text string) (string, bool) {
	return proseWalk(text, tsMarkers, tsQuotes)
}

func svelteProse(zone svelteZone, text string) (string, bool) {
	switch zone {
	case zoneScript:
		return proseWalk(text, svelteScriptMarks, tsQuotes)
	case zoneStyle:
		return proseWalk(text, svelteStyleMarks, svelteQuotes)
	case zoneMarkup:
	}
	if found, ok := proseWalk(text, svelteMarkupMarks, nil); ok {
		return found, true
	}
	return proseWalk(text, svelteExprMarks, svelteQuotes)
}

func svelteZoneAfter(zone svelteZone, line string) svelteZone {
	switch {
	case strings.Contains(line, "<script"):
		return zoneScript
	case strings.Contains(line, "</script>"):
		return zoneMarkup
	case strings.Contains(line, "<style"):
		return zoneStyle
	case strings.Contains(line, "</style>"):
		return zoneMarkup
	}
	return zone
}

func pythonProse(text string) (string, bool) {
	return proseWalk(text, pythonMarkers, pythonQuotes)
}

func proseWalk(text string, markers, quotes []string) (string, bool) {
	for i := 0; i < len(text); {
		if hasAnyPrefix(text[i:], markers...) {
			return text[i:], true
		}
		next := i + 1
		for _, q := range quotes {
			if strings.HasPrefix(text[i:], q) {
				next = skipQuoted(text, i+len(q), q)
				break
			}
		}
		i = next
	}
	return "", false
}

func skipQuoted(text string, start int, quote string) int {
	for i := start; i < len(text); i++ {
		if text[i] == '\\' {
			i++
			continue
		}
		if strings.HasPrefix(text[i:], quote) {
			return i + len(quote)
		}
	}
	return len(text)
}

func hasAnyPrefix(text string, prefixes ...string) bool {
	for _, p := range prefixes {
		if strings.HasPrefix(text, p) {
			return true
		}
	}
	return false
}

func repoFiles(root string) ([]string, error) {
	var files []string
	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		rel = filepath.ToSlash(rel)
		if d.IsDir() {
			if rel != "." && skipDir(d.Name(), rel) {
				return fs.SkipDir
			}
			return nil
		}
		if scanned[filepath.Ext(rel)] {
			files = append(files, rel)
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("commentgate: walk repository: %w", err)
	}
	return files, nil
}

func skipDir(name, rel string) bool {
	if skipAnywhere[name] || slices.Contains(skipPaths, rel) {
		return true
	}
	return !strings.Contains(rel, "/") && skipAtRoot[rel]
}

func TestScanGo(t *testing.T) {
	lineNotice := "// Copyright 2026 Example Corp. Licensed under the MIT License."
	blockNotice := "/*\nCopyright 2026 Example Corp.\nLicensed under the MIT License.\n*/"
	multiNotice := "// Copyright 2026 Example Corp.\n// Licensed under the MIT License.\n"
	tests := []struct {
		name    string
		file    string
		notices []legalRecord
		source  []string
		want    []finding
	}{
		{
			name:   "prose line comment",
			file:   "p.go",
			source: []string{"package p", "", "// Prose explains the function.", "func F() {}"},
			want:   []finding{{"p.go", 3, "// Prose explains the function."}},
		},
		{
			name: "allowed directives",
			file: "d.go",
			source: []string{
				"//go:build unix", "package d", "//go:embed prompts/system.md", "var system string",
				"//go:generate go run ./gen", "//nolint:gosec // reason stated here", "var x = 1",
			},
		},
		{
			name:   "nolint without reason",
			file:   "n.go",
			source: []string{"//nolint:gosec", "var x = 1"},
			want:   []finding{{"n.go", 1, "//nolint:gosec"}},
		},
		{
			name:   "generated header is a prose comment",
			file:   "g.go",
			source: []string{"// Code generated by a tool. DO NOT EDIT.", "package g"},
			want:   []finding{{"g.go", 1, "// Code generated by a tool. DO NOT EDIT."}},
		},
		{
			name:   "marker inside a string",
			file:   "s.go",
			source: []string{"package s", `var u = "https://example.com // not a comment"`},
		},
		{
			name:   "block comment",
			file:   "b.go",
			source: []string{"/* block */", "package b"},
			want:   []finding{{"b.go", 1, "/* block */"}},
		},
		{
			name:   "trailing prose comment",
			file:   "t.go",
			source: []string{"var x = 1 // why"},
			want:   []finding{{"t.go", 1, "// why"}},
		},
		{
			name:    "go line legal notice alone",
			file:    "legal.go",
			notices: []legalRecord{{"legal.go", lineNotice}},
			source:  []string{"package legal", "", "// Copyright 2026 Example Corp. Licensed under the MIT License.", "var X = 1"},
		},
		{
			name:    "go line legal notice with ordinary comment",
			file:    "legal.go",
			notices: []legalRecord{{"legal.go", lineNotice}},
			source: []string{
				"package legal", "", "// Copyright 2026 Example Corp. Licensed under the MIT License.",
				"// explains the code", "var X = 1",
			},
			want: []finding{{"legal.go", 4, "// explains the code"}},
		},
		{
			name:    "go line legal notice with trailing explanation",
			file:    "legal.go",
			notices: []legalRecord{{"legal.go", lineNotice}},
			source: []string{
				"package legal", "",
				"// Copyright 2026 Example Corp. Licensed under the MIT License. // explains",
				"var X = 1",
			},
			want: []finding{{"legal.go", 3, "// Copyright 2026 Example Corp. Licensed under the MIT License. // explains"}},
		},
		{
			name:    "go line legal keyword in ordinary prose",
			file:    "legal.go",
			notices: []legalRecord{{"legal.go", lineNotice}},
			source:  []string{"package legal", "", "// Copyright appears as a plain word in this sentence.", "var X = 1"},
			want:    []finding{{"legal.go", 3, "// Copyright appears as a plain word in this sentence."}},
		},
		{
			name:    "go line legal notice at unregistered path",
			file:    "other.go",
			notices: []legalRecord{{"legal.go", lineNotice}},
			source:  []string{"package other", "", "// Copyright 2026 Example Corp. Licensed under the MIT License.", "var X = 1"},
			want:    []finding{{"other.go", 3, "// Copyright 2026 Example Corp. Licensed under the MIT License."}},
		},
		{
			name:    "go block legal notice alone",
			file:    "legalb.go",
			notices: []legalRecord{{"legalb.go", blockNotice}},
			source: []string{
				"package legalb", "", "/*", "Copyright 2026 Example Corp.",
				"Licensed under the MIT License.", "*/", "var X = 1",
			},
		},
		{
			name:    "go block legal notice with ordinary comment",
			file:    "legalb.go",
			notices: []legalRecord{{"legalb.go", blockNotice}},
			source: []string{
				"package legalb", "", "/*", "Copyright 2026 Example Corp.",
				"Licensed under the MIT License.", "*/", "// explains the code", "var X = 1",
			},
			want: []finding{{"legalb.go", 7, "// explains the code"}},
		},
		{
			name:    "go block legal notice with trailing explanation",
			file:    "legalb.go",
			notices: []legalRecord{{"legalb.go", blockNotice}},
			source: []string{
				"package legalb", "", "/*", "Copyright 2026 Example Corp.",
				"Licensed under the MIT License.", "*/ // explains", "var X = 1",
			},
			want: []finding{{"legalb.go", 6, "// explains"}},
		},
		{
			name:    "go block legal keyword in ordinary prose",
			file:    "legalb.go",
			notices: []legalRecord{{"legalb.go", blockNotice}},
			source:  []string{"package legalb", "", "/* Copyright is a keyword in ordinary prose. */", "var X = 1"},
			want:    []finding{{"legalb.go", 3, "/* Copyright is a keyword in ordinary prose. */"}},
		},
		{
			name:    "go block legal notice at unregistered path",
			file:    "otherb.go",
			notices: []legalRecord{{"legalb.go", blockNotice}},
			source: []string{
				"package otherb", "", "/*", "Copyright 2026 Example Corp.",
				"Licensed under the MIT License.", "*/", "var X = 1",
			},
			want: []finding{{"otherb.go", 3, "/*\nCopyright 2026 Example Corp.\nLicensed under the MIT License.\n*/"}},
		},
		{
			name:    "go multi-line legal notice alone",
			file:    "legalm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source: []string{
				"package legalm", "", "// Copyright 2026 Example Corp.", "// Licensed under the MIT License.", "var X = 1",
			},
		},
		{
			name:    "go multi-line legal notice with ordinary comment",
			file:    "legalm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source: []string{
				"package legalm", "", "// Copyright 2026 Example Corp.", "// Licensed under the MIT License.",
				"// explains the code", "var X = 1",
			},
			want: []finding{{"legalm.go", 5, "// explains the code"}},
		},
		{
			name:    "go multi-line legal notice with trailing explanation",
			file:    "legalm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source: []string{
				"package legalm", "", "// Copyright 2026 Example Corp.", "// Licensed under the MIT License. // explains", "var X = 1",
			},
			want: []finding{
				{"legalm.go", 3, "// Copyright 2026 Example Corp."},
				{"legalm.go", 4, "// Licensed under the MIT License. // explains"},
			},
		},
		{
			name:    "go multi-line legal notice interrupted by prose",
			file:    "legalm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source: []string{
				"package legalm", "", "// Copyright 2026 Example Corp.", "// explains the code",
				"// Licensed under the MIT License.", "var X = 1",
			},
			want: []finding{
				{"legalm.go", 3, "// Copyright 2026 Example Corp."},
				{"legalm.go", 4, "// explains the code"},
				{"legalm.go", 5, "// Licensed under the MIT License."},
			},
		},
		{
			name:    "go multi-line legal notice cut short",
			file:    "legalm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source:  []string{"package legalm", "", "// Copyright 2026 Example Corp.", "var X = 1"},
			want:    []finding{{"legalm.go", 3, "// Copyright 2026 Example Corp."}},
		},
		{
			name:    "go multi-line legal notice at unregistered path",
			file:    "otherm.go",
			notices: []legalRecord{{"legalm.go", multiNotice}},
			source: []string{
				"package otherm", "", "// Copyright 2026 Example Corp.", "// Licensed under the MIT License.", "var X = 1",
			},
			want: []finding{
				{"otherm.go", 3, "// Copyright 2026 Example Corp."},
				{"otherm.go", 4, "// Licensed under the MIT License."},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			src := []byte(strings.Join(tt.source, "\n") + "\n")
			got := scanGo(tt.file, src, tt.notices)
			if !slices.Equal(got, tt.want) {
				t.Fatalf("scanGo(%s) = %+v, want %+v", tt.file, got, tt.want)
			}
		})
	}
}

func TestScanLines(t *testing.T) {
	slashNotice := "// Copyright 2026 Example Corp.\n// Licensed under the MIT License.\n"
	hashNotice := "# Copyright 2026 Example Corp.\n# Licensed under the MIT License.\n"
	blockNotice := "/*\nCopyright 2026 Example Corp.\nLicensed under the MIT License.\n*/\n"
	tests := []struct {
		name    string
		file    string
		notices []legalRecord
		source  []string
		want    []finding
	}{
		{
			name:   "svelte comment after a tab",
			file:   "x.svelte",
			source: []string{"<script>", "\t// Invalidate.", "</script>"},
			want:   []finding{{"x.svelte", 2, "// Invalidate."}},
		},
		{
			name:   "typescript block comment",
			file:   "x.ts",
			source: []string{"/* block */", "export const a = 1;"},
			want:   []finding{{"x.ts", 1, "/* block */"}},
		},
		{
			name:   "python shebang and comments",
			file:   "x.py",
			source: []string{"#!/usr/bin/env python3", "# Prose.", `"""Docstring."""`, "x = 1"},
			want: []finding{
				{"x.py", 1, "#!/usr/bin/env python3"},
				{"x.py", 2, "# Prose."},
				{"x.py", 3, `"""Docstring."""`},
			},
		},
		{
			name:   "python shebang outside line 1",
			file:   "y.py",
			source: []string{"x = 1", "#!/usr/bin/env python3"},
			want:   []finding{{"y.py", 2, "#!/usr/bin/env python3"}},
		},
		{
			name:   "svelte html comment",
			file:   "h.svelte",
			source: []string{"<!-- note -->", "<p>x</p>"},
			want:   []finding{{"h.svelte", 1, "<!-- note -->"}},
		},
		{
			name:   "svelte-ignore directive is a comment",
			file:   "i.svelte",
			source: []string{"<!-- svelte-ignore a11y_no_noninteractive_tabindex -->", "<div tabindex=\"0\"></div>"},
			want:   []finding{{"i.svelte", 1, "<!-- svelte-ignore a11y_no_noninteractive_tabindex -->"}},
		},
		{
			name:   "svelte script trailing comment",
			file:   "s.svelte",
			source: []string{`<script lang="ts">`, "\tlet a = 1; // note", "</script>"},
			want:   []finding{{"s.svelte", 2, "// note"}},
		},
		{
			name:   "svelte markup expression block comment",
			file:   "m.svelte",
			source: []string{"<p>{a /* note */}</p>"},
			want:   []finding{{"m.svelte", 1, "/* note */}</p>"}},
		},
		{
			name:   "svelte markup slashes and quoted marker stay clean",
			file:   "u.svelte",
			source: []string{`<a href="https://example.com">https://example.com</a>`, `<i title="a /* b */ c"></i>`},
		},
		{
			name:   "svelte style block comment",
			file:   "c.svelte",
			source: []string{"<style>", ".a { color: red; } /* note */", "</style>"},
			want:   []finding{{"c.svelte", 2, "/* note */"}},
		},
		{
			name:   "python directive prefixes are comments",
			file:   "d.py",
			source: []string{"# noqa", "# type: ignore[attr-defined]", "# pragma: no cover", "# noqualification needed", "# pragma: explanatory prose"},
			want: []finding{
				{"d.py", 1, "# noqa"},
				{"d.py", 2, "# type: ignore[attr-defined]"},
				{"d.py", 3, "# pragma: no cover"},
				{"d.py", 4, "# noqualification needed"},
				{"d.py", 5, "# pragma: explanatory prose"},
			},
		},
		{
			name:   "typescript code line",
			file:   "c.ts",
			source: []string{`const u = "//not a comment";`},
		},
		{
			name:   "js trailing comment",
			file:   "x.js",
			source: []string{"const a = 1; // note"},
			want:   []finding{{"x.js", 1, "// note"}},
		},
		{
			name:   "tsx line comment",
			file:   "y.tsx",
			source: []string{"export const a = 1; // note"},
			want:   []finding{{"y.tsx", 1, "// note"}},
		},
		{
			name:   "typescript trailing comment",
			file:   "z.ts",
			source: []string{`const u = "https://example.com"; // note`},
			want:   []finding{{"z.ts", 1, "// note"}},
		},
		{
			name:   "python trailing comment",
			file:   "p.py",
			source: []string{"x = 1  # note"},
			want:   []finding{{"p.py", 1, "# note"}},
		},
		{
			name:   "python string with hash stays clean",
			file:   "q.py",
			source: []string{`u = "a#b"`},
		},
		{
			name:   "backtick template with marker stays clean",
			file:   "t.ts",
			source: []string{"const u = `a // b`; const v = 1;"},
		},
		{
			name:   "ts expect-error directive is a comment",
			file:   "e.ts",
			source: []string{"// @ts-expect-error"},
			want:   []finding{{"e.ts", 1, "// @ts-expect-error"}},
		},
		{
			name:   "ts ignore directive is a comment",
			file:   "i.ts",
			source: []string{"// @ts-ignore"},
			want:   []finding{{"i.ts", 1, "// @ts-ignore"}},
		},
		{
			name:   "biome ignore directive is a comment",
			file:   "b.ts",
			source: []string{"// biome-ignore"},
			want:   []finding{{"b.ts", 1, "// biome-ignore"}},
		},
		{
			name:   "js leading comment",
			file:   "lead.js",
			source: []string{"// note", "const a = 1;"},
			want:   []finding{{"lead.js", 1, "// note"}},
		},
		{
			name:   "mjs leading and trailing comments",
			file:   "leadtrail.mjs",
			source: []string{"// lead", "const a = 1; // trail"},
			want:   []finding{{"leadtrail.mjs", 1, "// lead"}, {"leadtrail.mjs", 2, "// trail"}},
		},
		{
			name:   "cjs leading and trailing comments",
			file:   "leadtrail.cjs",
			source: []string{"// lead", "const a = 1; // trail"},
			want:   []finding{{"leadtrail.cjs", 1, "// lead"}, {"leadtrail.cjs", 2, "// trail"}},
		},
		{
			name:   "jsx leading and trailing comments",
			file:   "leadtrail.jsx",
			source: []string{"// lead", "const a = 1; // trail"},
			want:   []finding{{"leadtrail.jsx", 1, "// lead"}, {"leadtrail.jsx", 2, "// trail"}},
		},
		{
			name:   "tsx leading comment",
			file:   "lead.tsx",
			source: []string{"// lead", "export const a = 1;"},
			want:   []finding{{"lead.tsx", 1, "// lead"}},
		},
		{
			name:    "python legal notice alone",
			file:    "legal.py",
			notices: []legalRecord{{"legal.py", hashNotice}},
			source:  []string{"# Copyright 2026 Example Corp.", "# Licensed under the MIT License.", "x = 1"},
		},
		{
			name:    "python legal notice with ordinary comment",
			file:    "legal.py",
			notices: []legalRecord{{"legal.py", hashNotice}},
			source: []string{
				"# Copyright 2026 Example Corp.", "# Licensed under the MIT License.",
				"# explains", "x = 1",
			},
			want: []finding{{"legal.py", 3, "# explains"}},
		},
		{
			name:    "python legal notice with trailing explanation",
			file:    "legal.py",
			notices: []legalRecord{{"legal.py", hashNotice}},
			source: []string{
				"# Copyright 2026 Example Corp.", "# Licensed under the MIT License. # explains", "x = 1",
			},
			want: []finding{
				{"legal.py", 1, "# Copyright 2026 Example Corp."},
				{"legal.py", 2, "# Licensed under the MIT License. # explains"},
			},
		},
		{
			name:    "python legal keyword in ordinary prose",
			file:    "legal.py",
			notices: []legalRecord{{"legal.py", hashNotice}},
			source: []string{
				"# Copyright 2026 Example Corp.", "# Licensed under the MIT License.",
				"# Copyright appears as plain prose.", "x = 1",
			},
			want: []finding{{"legal.py", 3, "# Copyright appears as plain prose."}},
		},
		{
			name:    "python legal notice at unregistered path",
			file:    "other.py",
			notices: []legalRecord{{"legal.py", hashNotice}},
			source:  []string{"# Copyright 2026 Example Corp.", "# Licensed under the MIT License.", "x = 1"},
			want: []finding{
				{"other.py", 1, "# Copyright 2026 Example Corp."},
				{"other.py", 2, "# Licensed under the MIT License."},
			},
		},
		{
			name:    "typescript legal notice alone",
			file:    "legal.ts",
			notices: []legalRecord{{"legal.ts", slashNotice}},
			source:  []string{"// Copyright 2026 Example Corp.", "// Licensed under the MIT License.", "export const x = 1;"},
		},
		{
			name:    "typescript legal notice with ordinary comment",
			file:    "legal.ts",
			notices: []legalRecord{{"legal.ts", slashNotice}},
			source: []string{
				"// Copyright 2026 Example Corp.", "// Licensed under the MIT License.",
				"// explains", "export const x = 1;",
			},
			want: []finding{{"legal.ts", 3, "// explains"}},
		},
		{
			name:    "typescript legal notice with trailing explanation",
			file:    "legal.ts",
			notices: []legalRecord{{"legal.ts", slashNotice}},
			source: []string{
				"// Copyright 2026 Example Corp.", "// Licensed under the MIT License. // explains", "export const x = 1;",
			},
			want: []finding{
				{"legal.ts", 1, "// Copyright 2026 Example Corp."},
				{"legal.ts", 2, "// Licensed under the MIT License. // explains"},
			},
		},
		{
			name:    "typescript legal keyword in ordinary prose",
			file:    "legal.ts",
			notices: []legalRecord{{"legal.ts", slashNotice}},
			source: []string{
				"// Copyright 2026 Example Corp.", "// Licensed under the MIT License.",
				"// Copyright appears as plain prose.", "export const x = 1;",
			},
			want: []finding{{"legal.ts", 3, "// Copyright appears as plain prose."}},
		},
		{
			name:    "typescript legal notice at unregistered path",
			file:    "other.ts",
			notices: []legalRecord{{"legal.ts", slashNotice}},
			source:  []string{"// Copyright 2026 Example Corp.", "// Licensed under the MIT License.", "export const x = 1;"},
			want: []finding{
				{"other.ts", 1, "// Copyright 2026 Example Corp."},
				{"other.ts", 2, "// Licensed under the MIT License."},
			},
		},
		{
			name:    "typescript block legal notice alone",
			file:    "legalb.ts",
			notices: []legalRecord{{"legalb.ts", blockNotice}},
			source: []string{
				"/*", "Copyright 2026 Example Corp.", "Licensed under the MIT License.", "*/", "export const x = 1;",
			},
		},
		{
			name:    "typescript block legal notice with trailing explanation",
			file:    "legalb.ts",
			notices: []legalRecord{{"legalb.ts", blockNotice}},
			source: []string{
				"/*", "Copyright 2026 Example Corp.", "Licensed under the MIT License.", "*/ // explains", "export const x = 1;",
			},
			want: []finding{{"legalb.ts", 1, "/*"}, {"legalb.ts", 4, "// explains"}},
		},
		{
			name:    "typescript block legal notice at unregistered path",
			file:    "otherb.ts",
			notices: []legalRecord{{"legalb.ts", blockNotice}},
			source: []string{
				"/*", "Copyright 2026 Example Corp.", "Licensed under the MIT License.", "*/", "export const x = 1;",
			},
			want: []finding{{"otherb.ts", 1, "/*"}},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			src := []byte(strings.Join(tt.source, "\n") + "\n")
			got := scanLines(tt.file, src, tt.notices)
			if !slices.Equal(got, tt.want) {
				t.Fatalf("scanLines(%s) = %+v, want %+v", tt.file, got, tt.want)
			}
		})
	}
}

func TestScanLinesScriptLegalControls(t *testing.T) {
	notice := "// Copyright 2026 Example Corp.\n// Licensed under the MIT License.\n"
	head := []string{"// Copyright 2026 Example Corp.", "// Licensed under the MIT License."}
	for _, ext := range []string{".js", ".mjs", ".cjs", ".jsx", ".tsx"} {
		file := "legal" + ext
		other := "other" + ext
		tests := []struct {
			name   string
			file   string
			source []string
			want   []finding
		}{
			{
				name:   "notice alone",
				file:   file,
				source: slices.Concat(head, []string{"export const x = 1;"}),
			},
			{
				name:   "notice with ordinary comment",
				file:   file,
				source: slices.Concat(head, []string{"// explains", "export const x = 1;"}),
				want:   []finding{{file, 3, "// explains"}},
			},
			{
				name:   "notice with trailing comment after code",
				file:   file,
				source: slices.Concat(head, []string{"export const x = 1; // explains"}),
				want:   []finding{{file, 3, "// explains"}},
			},
			{
				name:   "notice with trailing explanation",
				file:   file,
				source: []string{head[0], head[1] + " // explains", "export const x = 1;"},
				want:   []finding{{file, 1, head[0]}, {file, 2, head[1] + " // explains"}},
			},
			{
				name:   "notice keyword in ordinary prose",
				file:   file,
				source: slices.Concat(head, []string{"// Copyright appears as plain prose.", "export const x = 1;"}),
				want:   []finding{{file, 3, "// Copyright appears as plain prose."}},
			},
			{
				name:   "notice at unregistered path",
				file:   other,
				source: slices.Concat(head, []string{"export const x = 1;"}),
				want:   []finding{{other, 1, head[0]}, {other, 2, head[1]}},
			},
		}
		for _, tt := range tests {
			t.Run(ext+" "+tt.name, func(t *testing.T) {
				src := []byte(strings.Join(tt.source, "\n") + "\n")
				got := scanLines(tt.file, src, []legalRecord{{file, notice}})
				if !slices.Equal(got, tt.want) {
					t.Fatalf("scanLines(%s) = %+v, want %+v", tt.file, got, tt.want)
				}
			})
		}
	}
}

func TestRepoFilesSkipsIgnoredDirectories(t *testing.T) {
	tests := []struct {
		name  string
		files []string
		want  []string
	}{
		{
			name: "ignored trees are skipped",
			files: []string{
				"a.go", "internal/b.go", ".references/c.go", "node_modules/d.ts", "web/node_modules/e.ts",
				"web/.svelte-kit/f.ts", "dist/g.py", "internal/webui/dist/h.ts", "web/src/lib/codex/i.ts",
				"web/src/lib/j.ts", "local/k.py", "docs/spec/l.md",
			},
			want: []string{"a.go", "internal/b.go", "web/src/lib/j.ts"},
		},
		{
			name:  "root-only names are kept below the root",
			files: []string{"internal/dist/m.go", "dist/n.go"},
			want:  []string{"internal/dist/m.go"},
		},
		{
			name: "script extensions are collected",
			files: []string{
				"a.js", "b.mjs", "c.cjs", "d.jsx", "e.tsx",
				"node_modules/f.js", "web/src/lib/codex/g.js", "internal/webui/dist/h.js",
			},
			want: []string{"a.js", "b.mjs", "c.cjs", "d.jsx", "e.tsx"},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			root := t.TempDir()
			writeTree(t, root, tt.files)
			got, err := repoFiles(root)
			if err != nil {
				t.Fatal(err)
			}
			if !slices.Equal(got, tt.want) {
				t.Fatalf("repoFiles = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestRepositoryHasNoProseComments(t *testing.T) {
	root := filepath.Join("..", "..")
	files, err := repoFiles(root)
	if err != nil {
		t.Fatal(err)
	}
	for _, rel := range files {
		src, err := os.ReadFile(filepath.Join(root, filepath.FromSlash(rel)))
		if err != nil {
			t.Fatal(err)
		}
		var found []finding
		if filepath.Ext(rel) == ".go" {
			found = scanGo(rel, src, legalNotices)
		} else {
			found = scanLines(rel, src, legalNotices)
		}
		for _, f := range found {
			t.Errorf("%s:%d: comment; restructure so a name or a type says it, or move its rule to docs/spec.", f.path, f.line)
		}
	}
}

func writeTree(t *testing.T, root string, files []string) {
	t.Helper()
	for _, rel := range files {
		path := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("x\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}
