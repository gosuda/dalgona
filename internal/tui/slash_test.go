package tui

import (
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestSlashPrefixedPromptCanBeSent(t *testing.T) {
	t.Parallel()
	tests := []struct {
		input string
		want  string
	}{
		{input: "//etc is odd", want: "/etc is odd"},
		{input: "/etc/hosts is wrong", want: "/etc/hosts is wrong"},
	}
	for _, test := range tests {
		t.Run(test.input, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newModel(caller, Options{}, nil)
			m.thread.id = "t1"
			m.input.composer.SetValue(test.input)
			command := m.submitPrompt()
			if command == nil {
				t.Fatalf("%q produced no RPC command", test.input)
			}
			command()
			last := caller.calls[len(caller.calls)-1]
			params, ok := last.params.(rpc.TurnStartParams)
			if last.method != "turn/start" || !ok || params.Input[0].Text != test.want {
				t.Fatalf("%q sent %s %#v, want turn/start with %q", test.input, last.method, last.params, test.want)
			}
		})
	}
}

func TestUnknownSlashStaysInEditor(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.input.composer.SetValue("/modle")
	if command := m.submitPrompt(); command != nil {
		t.Fatal("an unknown command produced an RPC command")
	}
	if len(caller.calls) != 0 {
		t.Fatalf("an unknown command made calls: %#v", caller.calls)
	}
	if got := m.input.composer.Value(); got != "/modle" {
		t.Fatalf("editor = %q, want /modle", got)
	}
	assertContains(t, m.notice, "start with //")
}

func TestParseSlashKinds(t *testing.T) {
	t.Parallel()
	tests := []struct {
		input string
		name  string
		arg   string
		kind  slashKind
	}{
		{input: "/model gpt-test", name: "model", arg: "gpt-test", kind: slashCommand},
		{input: "/advisor", name: "advisor", kind: slashCommand},
		{input: "/modle", name: "modle", kind: slashUnknown},
		{input: "/etc/hosts", arg: "/etc/hosts", kind: slashPrompt},
		{input: "//etc is odd", arg: "/etc is odd", kind: slashPrompt},
	}
	for _, test := range tests {
		name, arg, kind := parseSlash(test.input)
		if name != test.name || arg != test.arg || kind != test.kind {
			t.Fatalf("parseSlash(%q) = %q, %q, %v; want %q, %q, %v", test.input, name, arg, kind, test.name, test.arg, test.kind)
		}
	}
}
