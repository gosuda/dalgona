package pingate

import (
	"cmp"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

type finding struct {
	kind   string
	module string
	want   string
	got    string
	line   int
}

func (f finding) String() string {
	return fmt.Sprintf("dependencies.md:%d: %s: %s: want %s, got %s", f.line, f.kind, f.module, f.want, f.got)
}

type requirement struct {
	path    string
	version string
	direct  bool
}

type moduleFile struct {
	toolchain string
	requires  []requirement
	tools     []string
}

func parseGoMod(text string) moduleFile {
	var mod moduleFile
	block := ""
	for raw := range strings.SplitSeq(text, "\n") {
		line := strings.TrimSpace(raw)
		switch {
		case line == ")":
			block = ""
		case block == "require":
			mod.addRequirement(line)
		case block == "tool":
			mod.tools = append(mod.tools, line)
		default:
			block = mod.addLine(line, block)
		}
	}
	return mod
}

func (m *moduleFile) addLine(line, block string) string {
	keyword, rest, _ := strings.Cut(line, " ")
	rest = strings.TrimSpace(rest)
	switch keyword {
	case "require":
		if rest == "(" {
			return "require"
		}
		m.addRequirement(rest)
	case "tool":
		if rest == "(" {
			return "tool"
		}
		m.tools = append(m.tools, rest)
	case "toolchain":
		m.toolchain = rest
	}
	return block
}

func (m *moduleFile) addRequirement(line string) {
	fields := strings.Fields(line)
	if len(fields) < 2 {
		return
	}
	m.requires = append(m.requires, requirement{
		path:    fields[0],
		version: fields[1],
		direct:  !strings.Contains(line, "// indirect"),
	})
}

func (m moduleFile) moduleFor(tool string) (requirement, bool) {
	var best requirement
	found := false
	for _, req := range m.requires {
		if req.path != tool && !strings.HasPrefix(tool, req.path+"/") {
			continue
		}
		if !found || len(req.path) > len(best.path) {
			best, found = req, true
		}
	}
	return best, found
}

func (m moduleFile) expectedRows() []requirement {
	seen := make(map[string]bool, len(m.requires)+len(m.tools))
	var out []requirement
	add := func(req requirement) {
		if seen[req.path] {
			return
		}
		seen[req.path] = true
		out = append(out, req)
	}
	for _, req := range m.requires {
		if req.direct {
			add(req)
		}
	}
	for _, tool := range m.tools {
		if req, ok := m.moduleFor(tool); ok {
			add(requirement{path: req.path, version: req.version})
		}
	}
	return out
}

func (m moduleFile) moduleFindings(rows map[string][]pinRow, heading int) []finding {
	var out []finding
	for _, expected := range m.expectedRows() {
		pinned := rows[expected.path]
		if len(pinned) == 0 {
			out = append(out, finding{
				kind:   "missing-row",
				module: expected.path,
				want:   expected.version,
				got:    "none",
				line:   heading,
			})
			continue
		}
		if len(pinned) > 1 {
			out = append(out, finding{
				kind:   "duplicate-row",
				module: expected.path,
				want:   "one row",
				got:    fmt.Sprintf("%d rows", len(pinned)),
				line:   pinned[1].line,
			})
		}
		if pinned[0].pin != expected.version {
			out = append(out, finding{
				kind:   "version-mismatch",
				module: expected.path,
				want:   expected.version,
				got:    pinned[0].pin,
				line:   pinned[0].line,
			})
		}
	}
	return out
}

func (m moduleFile) toolchainFindings(rows map[string][]pinRow, heading int) []finding {
	pinned := rows["Go toolchain"]
	if len(pinned) == 0 {
		return []finding{{
			kind:   "toolchain-mismatch",
			module: "Go toolchain",
			want:   m.toolchain,
			got:    "none",
			line:   heading,
		}}
	}
	if pinned[0].pin != m.toolchain {
		return []finding{{
			kind:   "toolchain-mismatch",
			module: "Go toolchain",
			want:   m.toolchain,
			got:    pinned[0].pin,
			line:   pinned[0].line,
		}}
	}
	return nil
}

func (m moduleFile) toolFindings(heading int) []finding {
	var out []finding
	for _, tool := range m.tools {
		if _, ok := m.moduleFor(tool); ok {
			continue
		}
		out = append(out, finding{
			kind:   "tool-without-module",
			module: tool,
			want:   "a require line",
			got:    "none",
			line:   heading,
		})
	}
	return out
}

type pinRow struct {
	module string
	pin    string
	line   int
}

type pinTable struct {
	heading int
	rows    []pinRow
	second  int
	present bool
}

func parseDoc(text string) pinTable {
	lines := strings.Split(text, "\n")
	var table pinTable
	from, heading := groundSection(lines)
	table.heading = heading
	start, end, ok := pinRun(lines, from, sectionEnd(lines, from+1))
	if !ok {
		return table
	}
	table.present = true
	table.rows = rowsOf(lines, start+1, end)
	table.second = secondTable(lines, start, end)
	return table
}

func groundSection(lines []string) (int, int) {
	for i, line := range lines {
		if strings.TrimSpace(line) == "## Grounded pins" {
			return i, i + 1
		}
	}
	return 0, 1
}

func sectionEnd(lines []string, from int) int {
	for i := from; i < len(lines); i++ {
		if strings.HasPrefix(lines[i], "## ") {
			return i
		}
	}
	return len(lines)
}

func pinRun(lines []string, from, to int) (int, int, bool) {
	for i := from; i < to; {
		if !strings.HasPrefix(lines[i], "|") {
			i++
			continue
		}
		start := i
		for i < to && strings.HasPrefix(lines[i], "|") {
			i++
		}
		if isPinHeader(lines[start]) {
			return start, i, true
		}
	}
	return 0, 0, false
}

func secondTable(lines []string, skipStart, skipEnd int) int {
	for i, line := range lines {
		if i >= skipStart && i < skipEnd {
			continue
		}
		if isPinHeader(line) {
			return i + 1
		}
	}
	return 0
}

func rowsOf(lines []string, from, to int) []pinRow {
	var rows []pinRow
	for i := from; i < to; i++ {
		row := cells(lines[i])
		if len(row) < 2 || isSeparator(row) {
			continue
		}
		rows = append(rows, pinRow{
			module: strings.ReplaceAll(row[0], "`", ""),
			pin:    row[1],
			line:   i + 1,
		})
	}
	return rows
}

func isPinHeader(line string) bool {
	row := cells(line)
	return len(row) >= 2 && row[0] == "Module" && row[1] == "Pin"
}

func isSeparator(row []string) bool {
	if len(row) == 0 {
		return false
	}
	for _, cell := range row {
		if strings.Trim(cell, "-:") != "" {
			return false
		}
	}
	return true
}

func cells(line string) []string {
	parts := strings.Split(strings.TrimSpace(line), "|")
	if len(parts) > 0 && parts[0] == "" {
		parts = parts[1:]
	}
	if len(parts) > 0 && parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	out := make([]string, len(parts))
	for i, part := range parts {
		out[i] = strings.TrimSpace(part)
	}
	return out
}

func rowsByModule(table pinTable) map[string][]pinRow {
	rows := make(map[string][]pinRow, len(table.rows))
	for _, row := range table.rows {
		rows[row.module] = append(rows[row.module], row)
	}
	return rows
}

func (m moduleFile) extraFindings(table pinTable) []finding {
	known := map[string]bool{"Go toolchain": true}
	for _, req := range m.expectedRows() {
		known[req.path] = true
	}
	var out []finding
	for _, row := range table.rows {
		if known[row.module] {
			continue
		}
		out = append(out, finding{
			kind:   "extra-row",
			module: row.module,
			want:   "none",
			got:    row.pin,
			line:   row.line,
		})
	}
	return out
}

func check(goMod, doc string) []finding {
	mod := parseGoMod(goMod)
	table := parseDoc(doc)
	if !table.present {
		return []finding{{
			kind:   "no-table",
			module: "-",
			want:   "one table",
			got:    "none",
			line:   table.heading,
		}}
	}
	rows := rowsByModule(table)
	out := mod.moduleFindings(rows, table.heading)
	out = append(out, mod.toolchainFindings(rows, table.heading)...)
	out = append(out, mod.extraFindings(table)...)
	out = append(out, mod.toolFindings(table.heading)...)
	if table.second > 0 {
		out = append(out, finding{
			kind:   "second-table",
			module: "-",
			want:   "one table",
			got:    "second table",
			line:   table.second,
		})
	}
	slices.SortFunc(out, func(a, b finding) int {
		return cmp.Or(
			cmp.Compare(a.line, b.line),
			cmp.Compare(a.kind, b.kind),
			cmp.Compare(a.module, b.module),
		)
	})
	return out
}

const backtick = "`"

const cleanGoMod = `module github.com/gosuda/dalgona

go 1.27.0

toolchain go1.27.4

tool (
	github.com/evilmartians/lefthook/v2
)

require (
	charm.land/lipgloss/v2 v2.1.0
	github.com/charmbracelet/x/ansi v0.11.9 // indirect
	github.com/evilmartians/lefthook/v2 v2.2.2 // indirect
)
`

const cleanDoc = `# Dependencies

## Grounded pins

|Module|Pin|Released|Use|OSV|
|---|---|---|---|---|
|Go toolchain|go1.27.4|2026-11-10|toolchain|none|
|charm.land/lipgloss/v2|v2.1.0|2026-11-09|TUI|none|
|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|none|

## Rejected alternatives

|Choice|Replacement|Reason|
|---|---|---|
|example.com/sdk|net/http|reflection codec|
`

const noTableDoc = `# Dependencies

## Grounded pins

## Rejected alternatives

|Choice|Replacement|Reason|
|---|---|---|
|example.com/sdk|net/http|reflection codec|
`

func TestCheckPins(t *testing.T) {
	edit := func(text, old, replacement string) string {
		if !strings.Contains(text, old) {
			t.Fatalf("fixture does not contain %q", old)
		}
		return strings.Replace(text, old, replacement, 1)
	}
	const (
		lipglossRow = "|charm.land/lipgloss/v2|v2.1.0|2026-11-09|TUI|none|\n"
		lefthookRow = "|github.com/evilmartians/lefthook/v2|v2.2.2|2026-10-13|hook tool|none|\n"
		extraRow    = "|example.com/x|v1.0.0|2026-01-01|probe|none|\n"
		lefthookDep = "\tgithub.com/evilmartians/lefthook/v2\n"
		hookRequire = "\tgithub.com/evilmartians/lefthook/v2 v2.2.2 // indirect\n"
	)
	tests := []struct {
		name  string
		goMod string
		doc   string
		want  []finding
	}{
		{
			name:  "clean",
			goMod: cleanGoMod,
			doc:   cleanDoc,
		},
		{
			name: "indirect without row",
			goMod: edit(cleanGoMod, hookRequire,
				hookRequire+"\tgolang.org/x/sys v0.50.0 // indirect\n"),
			doc: cleanDoc,
		},
		{
			name: "tool under a required module",
			goMod: edit(edit(cleanGoMod, lefthookDep, lefthookDep+"\tgithub.com/golangci/golangci-lint/v2/cmd/golangci-lint\n"),
				hookRequire, hookRequire+"\tgithub.com/golangci/golangci-lint/v2 v2.15.0 // indirect\n"),
			doc: edit(cleanDoc, lefthookRow,
				lefthookRow+"|github.com/golangci/golangci-lint/v2|v2.15.0|2026-10-16|lint tool|none|\n"),
		},
		{
			name:  "backticked module cell",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, "|charm.land/lipgloss/v2|", "|"+backtick+"charm.land/lipgloss/v2"+backtick+"|"),
		},
		{
			name:  "missing row",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, lipglossRow, ""),
			want: []finding{{
				kind: "missing-row", module: "charm.land/lipgloss/v2",
				want: "v2.1.0", got: "none", line: 3,
			}},
		},
		{
			name:  "duplicate row",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, lipglossRow, lipglossRow+lipglossRow),
			want: []finding{{
				kind: "duplicate-row", module: "charm.land/lipgloss/v2",
				want: "one row", got: "2 rows", line: 9,
			}},
		},
		{
			name:  "version mismatch",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, "|charm.land/lipgloss/v2|v2.1.0|", "|charm.land/lipgloss/v2|v2.0.6|"),
			want: []finding{{
				kind: "version-mismatch", module: "charm.land/lipgloss/v2",
				want: "v2.1.0", got: "v2.0.6", line: 8,
			}},
		},
		{
			name:  "extra row",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, lefthookRow, lefthookRow+extraRow),
			want: []finding{{
				kind: "extra-row", module: "example.com/x",
				want: "none", got: "v1.0.0", line: 10,
			}},
		},
		{
			name:  "toolchain mismatch",
			goMod: cleanGoMod,
			doc:   edit(cleanDoc, "|Go toolchain|go1.27.4|", "|Go toolchain|go1.27.3|"),
			want: []finding{{
				kind: "toolchain-mismatch", module: "Go toolchain",
				want: "go1.27.4", got: "go1.27.3", line: 7,
			}},
		},
		{
			name:  "second table",
			goMod: cleanGoMod,
			doc:   cleanDoc + "|Module|Pin|\n|---|---|\n|example.com/y|v1.0.0|\n",
			want: []finding{{
				kind: "second-table", module: "-",
				want: "one table", got: "second table", line: 16,
			}},
		},
		{
			name: "tool without module",
			goMod: edit(cleanGoMod, lefthookDep,
				lefthookDep+"\tgolang.org/x/tools/cmd/stringer\n"),
			doc: cleanDoc,
			want: []finding{{
				kind: "tool-without-module", module: "golang.org/x/tools/cmd/stringer",
				want: "a require line", got: "none", line: 3,
			}},
		},
		{
			name:  "no table",
			goMod: cleanGoMod,
			doc:   noTableDoc,
			want: []finding{{
				kind: "no-table", module: "-",
				want: "one table", got: "none", line: 3,
			}},
		},
		{
			name:  "sorted output",
			goMod: cleanGoMod,
			doc: edit(edit(cleanDoc, "|Go toolchain|go1.27.4|", "|Go toolchain|go1.27.3|"),
				"|charm.land/lipgloss/v2|v2.1.0|", "|charm.land/lipgloss/v2|v2.0.6|"),
			want: []finding{
				{
					kind: "toolchain-mismatch", module: "Go toolchain",
					want: "go1.27.4", got: "go1.27.3", line: 7,
				},
				{
					kind: "version-mismatch", module: "charm.land/lipgloss/v2",
					want: "v2.1.0", got: "v2.0.6", line: 8,
				},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := check(tt.goMod, tt.doc); !slices.Equal(got, tt.want) {
				t.Fatalf("got %v, want %v", got, tt.want)
			}
		})
	}
}

func readText(t *testing.T, path string) string {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return strings.ReplaceAll(string(data), "\r\n", "\n")
}

func repoRoot(t *testing.T) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	for {
		mod, readErr := os.ReadFile(filepath.Join(dir, "go.mod"))
		if readErr == nil && strings.HasPrefix(string(mod), "module github.com/gosuda/dalgona") {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatalf("no go.mod for github.com/gosuda/dalgona above %s", dir)
		}
		dir = parent
	}
}

func TestDependencyTableMatchesGoMod(t *testing.T) {
	root := repoRoot(t)
	goMod := readText(t, filepath.Join(root, "go.mod"))
	doc := readText(t, filepath.Join(root, "docs", "decisions", "dependencies.md"))
	for _, f := range check(goMod, doc) {
		t.Error(f.String())
	}
}
