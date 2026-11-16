package search

import "testing"

const ownerFixture = "package a\n\ntype Router struct{}\n\nfunc (r Router) Handle() bool {\n\treturn true\n}\n\ntype Mux struct{}\n\nfunc (m Mux) Handle() bool {\n\treturn true\n}\n"

func TestDefinitionOwnerQualifiedPattern(t *testing.T) {
	root := t.TempDir()
	t.Chdir(root)
	writeSearchFixture(t, root, "a.go", ownerFixture)
	cases := []struct {
		name, args, want string
	}{
		{
			"qualified owner selects one method",
			`{"mode":"definition","pattern":"Router.Handle"}`,
			"a.go:5-7: method func (r Router) Handle() bool {\na.go:5: func (r Router) Handle() bool {\na.go:6: \treturn true\na.go:7: }",
		},
		{
			"other owner selects the other method",
			`{"mode":"definition","pattern":"Mux.Handle"}`,
			"a.go:11-13: method func (m Mux) Handle() bool {\na.go:11: func (m Mux) Handle() bool {\na.go:12: \treturn true\na.go:13: }",
		},
		{
			"plain name selects both methods",
			`{"mode":"definition","pattern":"Handle"}`,
			"a.go:5-7: method func (r Router) Handle() bool {\na.go:5: func (r Router) Handle() bool {\na.go:6: \treturn true\na.go:7: }\na.go:11-13: method func (m Mux) Handle() bool {\na.go:11: func (m Mux) Handle() bool {\na.go:12: \treturn true\na.go:13: }",
		},
		{
			"owner name selects the type and not its method",
			`{"mode":"definition","pattern":"Router"}`,
			"a.go:3-3: type type Router struct{}\na.go:3: type Router struct{}",
		},
		{
			"symbols plain name still finds the method",
			`{"mode":"symbols","pattern":"Handle"}`,
			"a.go:5-7: method func (r Router) Handle() bool {\na.go:11-13: method func (m Mux) Handle() bool {",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := runMain(t, tc.args).Text; got != tc.want {
				t.Fatalf("%s = %q, want %q", tc.name, got, tc.want)
			}
		})
	}
}

func TestSplitOwnerPatternRules(t *testing.T) {
	cases := []struct {
		raw         string
		owner, name string
		split       bool
	}{
		{"Router.Handle", "Router", "Handle", true},
		{`Router\.Handle`, "Router", "Handle", true},
		{"R.n", "R", "n", true},
		{"Router.", "", "", false},
		{"Router.H.", "", "", false},
		{".Handle", "", "", false},
		{"Router.Han dle", "", "", false},
		{"Router", "", "", false},
	}
	for _, tc := range cases {
		owner, name, split := splitOwnerPattern(tc.raw)
		if owner != tc.owner || name != tc.name || split != tc.split {
			t.Fatalf("splitOwnerPattern(%q) = %q, %q, %v, want %q, %q, %v", tc.raw, owner, name, split, tc.owner, tc.name, tc.split)
		}
	}
}
