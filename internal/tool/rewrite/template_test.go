package rewrite

import (
	"reflect"
	"testing"
)

func TestParseTemplate(t *testing.T) {
	const lone = "rewrite: replace has a lone @ at byte "
	cases := []struct {
		name  string
		in    string
		want  template
		names []string
		err   string
	}{
		{name: "escaped at", in: "a@@b", want: template{{text: "a@b"}}},
		{name: "plain capture", in: "@name", want: template{{capture: "name"}}, names: []string{"name"}},
		{name: "capture then dot", in: "x@name.y", want: template{{text: "x"}, {capture: "name"}, {text: ".y"}}, names: []string{"name"}},
		{name: "braced dotted capture", in: "@{definition.function}", want: template{{capture: "definition.function"}}, names: []string{"definition.function"}},
		{name: "trailing dot", in: "@name.", want: template{{capture: "name"}, {text: "."}}, names: []string{"name"}},
		{name: "escape before capture", in: "@@name", want: template{{text: "@name"}}},
		{name: "lone at with space", in: "@ x", err: lone + "0; write @@ for a literal @"},
		{name: "empty braces", in: "@{}", err: lone + "0; write @@ for a literal @"},
		{name: "unclosed braces", in: "@{name", err: lone + "0; write @@ for a literal @"},
		{name: "lone at mid text", in: "a @ b", err: lone + "2; write @@ for a literal @"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			checkParseTemplate(t, tc.in, tc.want, tc.names, tc.err)
		})
	}
}

func checkParseTemplate(t *testing.T, in string, want template, names []string, wantErr string) {
	t.Helper()
	got, err := parseTemplate(in)
	if wantErr != "" {
		if err == nil || err.Error() != wantErr {
			t.Fatalf("parseTemplate(%q) error = %v, want %q", in, err, wantErr)
		}
		return
	}
	if err != nil {
		t.Fatalf("parseTemplate(%q) error = %v", in, err)
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("parseTemplate(%q) = %#v, want %#v", in, got, want)
	}
	if !reflect.DeepEqual(got.captures(), names) {
		t.Fatalf("parseTemplate(%q) captures = %v, want %v", in, got.captures(), names)
	}
}

func TestExpandCaptureAndLiteralAt(t *testing.T) {
	src := []byte("func Hi() {}")
	g := group{line: 1, spans: map[string]span{"name": {start: 5, end: 7}}}
	tmpl, err := parseTemplate("@name@@")
	if err != nil {
		t.Fatal(err)
	}
	got, err := expand(tmpl, src, g)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "Hi@" {
		t.Fatalf("expand = %q, want %q", got, "Hi@")
	}
}
