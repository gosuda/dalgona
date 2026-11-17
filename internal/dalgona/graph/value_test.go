package graph

import (
	"encoding/json/jsontext"
	"fmt"
	"strings"
	"testing"
)

func mustParse(t *testing.T, raw string) Graph {
	t.Helper()
	graph, err := Parse(jsontext.Value(raw))
	if err != nil {
		t.Fatalf("Parse(%q) = %v", raw, err)
	}
	return graph
}

func nodeJSON(id string, extra ...string) string {
	parts := []string{fmt.Sprintf(`"id":%q,"prompt":"do work"`, id)}
	parts = append(parts, extra...)
	return "{" + strings.Join(parts, ",") + "}"
}

func graphJSON(nodes ...string) string {
	return `{"nodes":[` + strings.Join(nodes, ",") + `]}`
}

func chainNodes() []string {
	nodes := make([]string, 0, maxNodes)
	for i := 0; i < maxNodes; i++ {
		id := fmt.Sprintf("n%d", i)
		if i == 0 {
			nodes = append(nodes, nodeJSON(id))
		} else {
			nodes = append(nodes, nodeJSON(id, fmt.Sprintf(`"needs":["n%d"]`, i-1)))
		}
	}
	return nodes
}

func depNeeds() []string {
	needs := make([]string, 0, maxNeeds)
	for i := 0; i < maxNeeds; i++ {
		needs = append(needs, fmt.Sprintf(`"d%d"`, i))
	}
	return needs
}

func depNodes() []string {
	nodes := make([]string, 0, maxNeeds)
	for i := 0; i < maxNeeds; i++ {
		nodes = append(nodes, nodeJSON(fmt.Sprintf("d%d", i)))
	}
	return nodes
}

func pathWrites() []string {
	writes := make([]string, 0, maxWrites)
	for i := 0; i < maxWrites; i++ {
		writes = append(writes, fmt.Sprintf(`"w/%d"`, i))
	}
	return writes
}

func TestParseAcceptsValidGraphs(t *testing.T) {
	tests := []struct {
		name string
		raw  string
	}{
		{"single explorer node", graphJSON(nodeJSON("a"))},
		{
			"chain with needs profile writes schema",
			graphJSON(
				nodeJSON("a", `"writes":["src/a.txt"]`),
				nodeJSON("b", `"needs":["a"],"profile":"reviewer","writes":["src/b.txt"],"schema":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}`),
			),
		},
		{
			"ordered overlapping writes allowed",
			graphJSON(
				nodeJSON("a", `"writes":["src"]`),
				nodeJSON("b", `"needs":["a"],"writes":["src/deep/file.go"]`),
			),
		},
		{"whole workspace write", graphJSON(nodeJSON("a", `"writes":["."]`))},
		{"empty needs writes schema omitted", graphJSON(nodeJSON("a", `"needs":[],"writes":[]`))},
		{"maximal id length", graphJSON(nodeJSON("a" + strings.Repeat("b", 47)))},
		{"profile grammar", graphJSON(nodeJSON("a", `"profile":"worker-2_x"`))},
		{"maximal graph sixty four nodes", graphJSON(chainNodes()...)},
		{"sixty three needs", graphJSON(append([]string{nodeJSON("a", `"needs":[`+strings.Join(depNeeds(), ",")+`]`)}, depNodes()...)...)},
		{"sixty four writes", graphJSON(nodeJSON("a", `"writes":[`+strings.Join(pathWrites(), ",")+`]`))},
		{"max prompt", `{"nodes":[{"id":"a","prompt":"` + strings.Repeat("x", maxPromptBytes) + `"}]}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if _, err := Parse(jsontext.Value(tt.raw)); err != nil {
				t.Fatalf("Parse() = %v, want nil", err)
			}
		})
	}
}

func TestParsePreservesOrderAndCanonicalizes(t *testing.T) {
	raw := graphJSON(
		nodeJSON("zeta", `"needs":["b-id","a-id"],"writes":["z/last","a/first"]`),
		nodeJSON("alpha", `"needs":[],"writes":[]`),
		nodeJSON("b-id", `"needs":[]`),
		nodeJSON("a-id", `"needs":[]`),
	)
	graph := mustParse(t, raw)
	if len(graph.Nodes) != 4 {
		t.Fatalf("nodes = %d, want 4", len(graph.Nodes))
	}
	wantOrder := []string{"zeta", "alpha", "b-id", "a-id"}
	for i, want := range wantOrder {
		if graph.Nodes[i].ID != want {
			t.Fatalf("node %d id = %q, want %q (submission order)", i, graph.Nodes[i].ID, want)
		}
	}
	gotNeeds := graph.Nodes[0].Needs
	if gotNeeds[0] != "a-id" || gotNeeds[1] != "b-id" {
		t.Fatalf("needs = %v, want lexical [a-id b-id]", gotNeeds)
	}
	gotWrites := graph.Nodes[0].Writes
	if gotWrites[0] != "a/first" || gotWrites[1] != "z/last" {
		t.Fatalf("writes = %v, want lexical [a/first z/last]", gotWrites)
	}
}

func TestParseRejects(t *testing.T) {
	manyNodes := make([]string, 65)
	for i := range manyNodes {
		manyNodes[i] = nodeJSON(fmt.Sprintf("n%d", i))
	}
	sixtyFiveNeeds := make([]string, 65)
	for i := range sixtyFiveNeeds {
		sixtyFiveNeeds[i] = fmt.Sprintf(`"dep%d"`, i)
	}
	sixtyFiveWrites := make([]string, 65)
	for i := range sixtyFiveWrites {
		sixtyFiveWrites[i] = fmt.Sprintf(`"w/%d"`, i)
	}
	deepSchema := `"schema":` + strings.Repeat(`{"items":`, 17) + `{"type":"null"}` + strings.Repeat(`}`, 17)

	tests := rejectCases(manyNodes, sixtyFiveNeeds, sixtyFiveWrites, deepSchema)
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assertParseReject(t, tt.raw, tt.wantErr)
		})
	}
}

func assertParseReject(t *testing.T, raw, wantErr string) {
	t.Helper()
	_, err := Parse(jsontext.Value(raw))
	if wantErr == "" {
		if err == nil {
			t.Fatalf("Parse() = nil, want size error")
		}
		return
	}
	if err == nil {
		t.Fatalf("Parse() = nil, want %q", wantErr)
	}
	if err.Error() != wantErr {
		t.Fatalf("Parse() = %q, want %q", err.Error(), wantErr)
	}
}

func rejectCases(manyNodes, sixtyFiveNeeds, sixtyFiveWrites []string, deepSchema string) []struct {
	name    string
	raw     string
	wantErr string
} {
	return []struct {
		name    string
		raw     string
		wantErr string
	}{
		{"empty input", ``, "graph: invalid value"},
		{"malformed json", `{`, "graph: invalid value"},
		{"top level array", `[]`, "graph: invalid value"},
		{"top level string", `"x"`, "graph: invalid value"},
		{"trailing data", graphJSON(nodeJSON("a")) + ` {}`, "graph: invalid value"},
		{"missing nodes", `{}`, "graph: invalid value"},
		{"nodes null", `{"nodes":null}`, "graph: invalid value"},
		{"nodes not array", `{"nodes":{}}`, "graph: invalid value"},
		{"node not object", `{"nodes":[1]}`, "graph: invalid value"},
		{"duplicate top member", `{"nodes":[],"nodes":[]}`, "graph: duplicate member \"nodes\""},
		{"duplicate top member nodes", `{"nodes":[` + nodeJSON("a") + `],"nodes":[` + nodeJSON("b") + `]}`, "graph: duplicate member \"nodes\""},
		{"unknown top member", `{"nodes":[` + nodeJSON("a") + `],"extra":1}`, "graph: unknown member \"extra\""},
		{"duplicate node member", `{"nodes":[{"id":"a","id":"b","prompt":"x"}]}`, "graph: duplicate member \"id\""},
		{"unknown node member", `{"nodes":[{"id":"a","prompt":"x","runner":"local"}]}`, "graph: unknown member \"runner\""},
		{"zero nodes", `{"nodes":[]}`, "graph: nodes must contain 1 to 64 nodes"},
		{"sixty five nodes", graphJSON(manyNodes...), "graph: nodes must contain 1 to 64 nodes"},
		{"empty id", graphJSON(nodeJSON("")), "graph: invalid node id \"\""},
		{"uppercase id", graphJSON(nodeJSON("Abc")), "graph: invalid node id \"Abc\""},
		{"digit first id", graphJSON(nodeJSON("1abc")), "graph: invalid node id \"1abc\""},
		{"space in id", graphJSON(nodeJSON("a b")), "graph: invalid node id \"a b\""},
		{"id too long", graphJSON(nodeJSON("a" + strings.Repeat("b", 48))), "graph: invalid node id \"" + "a" + strings.Repeat("b", 48) + "\""},
		{"duplicate id", graphJSON(nodeJSON("a"), nodeJSON("a")), "graph: duplicate node id \"a\""},
		{"invalid profile", graphJSON(nodeJSON("a", `"profile":"Bad Profile"`)), "graph: invalid profile \"Bad Profile\""},
		{"empty prompt", `{"nodes":[{"id":"a","prompt":""}]}`, "graph: node \"a\" needs a prompt"},
		{"whitespace prompt", `{"nodes":[{"id":"a","prompt":"  \n\t "}]}`, "graph: node \"a\" needs a prompt"},
		{"oversized prompt", `{"nodes":[{"id":"a","prompt":"` + strings.Repeat("x", maxPromptBytes+1) + `"}]}`, "graph: node \"a\" prompt must be valid utf-8 within 65536 bytes"},
		{"needs not array", graphJSON(nodeJSON("a", `"needs":"b"`)), "graph: invalid value"},
		{"needs non-string", `{"nodes":[{"id":"a","prompt":"x","needs":[1]}]}`, "graph: invalid value"},
		{"too many needs", graphJSON(nodeJSON("a", `"needs":[`+strings.Join(sixtyFiveNeeds, ",")+`]`)), "graph: node \"a\" exceeds 63 needs"},
		{"duplicate need", graphJSON(nodeJSON("a", `"needs":["b","b"]`), nodeJSON("b")), "graph: node \"a\" repeats need \"b\""},
		{"self need", graphJSON(nodeJSON("a", `"needs":["a"]`)), "graph: node \"a\" needs itself"},
		{"unknown need", graphJSON(nodeJSON("a", `"needs":["ghost"]`)), "graph: node \"a\" has unknown need \"ghost\""},
		{"dependency cycle", graphJSON(nodeJSON("a", `"needs":["b"]`), nodeJSON("b", `"needs":["a"]`)), "graph: dependency cycle"},
		{"self cycle via third", graphJSON(nodeJSON("a", `"needs":["b"]`), nodeJSON("b", `"needs":["c"]`), nodeJSON("c", `"needs":["a"]`)), "graph: dependency cycle"},
		{"too many writes", graphJSON(nodeJSON("a", `"writes":[`+strings.Join(sixtyFiveWrites, ",")+`]`)), "graph: node \"a\" exceeds 64 writes"},
		{"writes not array", graphJSON(nodeJSON("a", `"writes":"x"`)), "graph: invalid value"},
		{"absolute write", graphJSON(nodeJSON("a", `"writes":["/etc/passwd"]`)), "graph: node \"a\" has invalid write path \"/etc/passwd\""},
		{"backslash write", graphJSON(nodeJSON("a", `"writes":["a\\b"]`)), "graph: node \"a\" has invalid write path \"a\\\\b\""},
		{"nul write", graphJSON(nodeJSON("a", `"writes":["a\u0000b"]`)), "graph: node \"a\" has invalid write path \"a\\x00b\""},
		{"control write", graphJSON(nodeJSON("a", `"writes":["a\u0007b"]`)), "graph: node \"a\" has invalid write path \"a\\ab\""},
		{"trailing slash write", graphJSON(nodeJSON("a", `"writes":["a/"]`)), "graph: node \"a\" has invalid write path \"a/\""},
		{"dot component write", graphJSON(nodeJSON("a", `"writes":["a/./b"]`)), "graph: node \"a\" has invalid write path \"a/./b\""},
		{"traversal write", graphJSON(nodeJSON("a", `"writes":["a/../b"]`)), "graph: node \"a\" has invalid write path \"a/../b\""},
		{"dotdot leading", graphJSON(nodeJSON("a", `"writes":["../x"]`)), "graph: node \"a\" has invalid write path \"../x\""},
		{"git component", graphJSON(nodeJSON("a", `"writes":["repo/.git/config"]`)), "graph: node \"a\" has invalid write path \"repo/.git/config\""},
		{"git component case", graphJSON(nodeJSON("a", `"writes":["repo/.GIT/x"]`)), "graph: node \"a\" has invalid write path \"repo/.GIT/x\""},
		{"duplicate write", graphJSON(nodeJSON("a", `"writes":["x","x"]`)), "graph: node \"a\" repeats write path \"x\""},
		{"unordered overlap equal", graphJSON(nodeJSON("a", `"writes":["src/x"]`), nodeJSON("b", `"writes":["src/x"]`)), "graph: unordered writes overlap between \"a\" and \"b\" at \"src/x\""},
		{"unordered overlap ancestor", graphJSON(nodeJSON("a", `"writes":["src"]`), nodeJSON("b", `"writes":["src/deep/f.go"]`)), "graph: unordered writes overlap between \"a\" and \"b\" at \"src\""},
		{"unordered overlap whole workspace", graphJSON(nodeJSON("a", `"writes":["."]`), nodeJSON("b", `"writes":["src/x"]`)), "graph: unordered writes overlap between \"a\" and \"b\" at \".\""},
		{"unordered overlap picks pair order", graphJSON(nodeJSON("a", `"writes":["m"]`), nodeJSON("b", `"writes":["z"]`), nodeJSON("c", `"writes":["m/1","z/1"]`)), "graph: unordered writes overlap between \"a\" and \"c\" at \"m\""},
		{"schema unsupported keyword", graphJSON(nodeJSON("a", `"schema":{"$ref":"http://x"}`)), "task: unsupported schema keyword \"$ref\""},
		{"schema unsupported nested", graphJSON(nodeJSON("a", `"schema":{"type":"object","properties":{"x":{"type":"string","pattern":"p"}}}`)), "task: unsupported schema keyword \"pattern\""},
		{"schema additionalProperties true", graphJSON(nodeJSON("a", `"schema":{"type":"object","additionalProperties":true}`)), "task: additionalProperties must be false"},
		{"schema additionalProperties schema", graphJSON(nodeJSON("a", `"schema":{"type":"object","additionalProperties":{"type":"string"}}`)), "task: additionalProperties must be false"},
		{"schema null", graphJSON(nodeJSON("a", `"schema":null`)), "graph: invalid value"},
		{"schema non object", graphJSON(nodeJSON("a", `"schema":"string"`)), "graph: invalid value"},
		{"schema bad type", graphJSON(nodeJSON("a", `"schema":{"type":"union"}`)), "graph: invalid value"},
		{"schema type array union", graphJSON(nodeJSON("a", `"schema":{"type":["string","null"]}`)), "graph: invalid value"},
		{"schema required undeclared", graphJSON(nodeJSON("a", `"schema":{"type":"object","properties":{"x":{"type":"string"}},"required":["y"]}`)), "graph: invalid value"},
		{"schema required no properties", graphJSON(nodeJSON("a", `"schema":{"type":"object","required":["y"]}`)), "graph: invalid value"},
		{"schema required duplicate", graphJSON(nodeJSON("a", `"schema":{"type":"object","properties":{"x":{"type":"string"}},"required":["x","x"]}`)), "graph: invalid value"},
		{"schema enum empty", graphJSON(nodeJSON("a", `"schema":{"enum":[]}`)), "graph: invalid value"},
		{"schema too deep", graphJSON(nodeJSON("a", deepSchema)), "task: output schema exceeds limit"},
		{"schema oversized", graphJSON(nodeJSON("a", `"schema":{"enum":[`+strings.Repeat(`"`+strings.Repeat("y", 100)+`",`, 200)+`null]}`)), "task: output schema exceeds limit"},
	}
}

func TestParseOversizedValue(t *testing.T) {
	raw := graphJSON(nodeJSON("a", `"prompt":"`+strings.Repeat("x", maxSerialized)+`"`))
	if len(raw) <= maxSerialized {
		t.Fatalf("test raw too small: %d", len(raw))
	}
	_, err := Parse(jsontext.Value(raw))
	if err == nil || err.Error() != "graph: value exceeds 1048576 bytes" {
		t.Fatalf("Parse() = %v, want size error", err)
	}
}

func TestValidateRejects(t *testing.T) {
	tests := []struct {
		name    string
		value   Graph
		wantErr string
	}{
		{"zero nodes", Graph{}, "graph: nodes must contain 1 to 64 nodes"},
		{"invalid utf8 prompt", Graph{Nodes: []Node{{ID: "a", Prompt: string([]byte{0xff, 0xfe})}}}, "graph: node \"a\" prompt must be valid utf-8 within 65536 bytes"},
		{"invalid utf8 write", Graph{Nodes: []Node{{ID: "a", Prompt: "x", Writes: []string{string([]byte{0xff})}}}}, "graph: node \"a\" has invalid write path \"\\xff\""},
		{"control write via validate", Graph{Nodes: []Node{{ID: "a", Prompt: "x", Writes: []string{"a\u0085b"}}}}, "graph: node \"a\" has invalid write path \"a\\u0085b\""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := Validate(tt.value)
			if err == nil {
				t.Fatalf("Validate() = nil, want %q", tt.wantErr)
			}
			if err.Error() != tt.wantErr {
				t.Fatalf("Validate() = %q, want %q", err.Error(), tt.wantErr)
			}
		})
	}
}

func TestValidateAcceptsConstructed(t *testing.T) {
	graph := Graph{Nodes: []Node{
		{ID: "a", Prompt: "first", Writes: []string{"src"}},
		{ID: "b", Prompt: "second", Needs: []string{"a"}, Writes: []string{"src/ok"}},
	}}
	if err := Validate(graph); err != nil {
		t.Fatalf("Validate() = %v, want nil", err)
	}
}

func TestErrorOrdering(t *testing.T) {
	localErr := graphJSON(
		nodeJSON("a", `"needs":["ghost"]`),
		nodeJSON("B", `"needs":[]`),
	)
	_, err := Parse(jsontext.Value(localErr))
	if err == nil || err.Error() != "graph: invalid node id \"B\"" {
		t.Fatalf("node-local must precede unknown need, got %v", err)
	}

	cycleAndOverlap := graphJSON(
		nodeJSON("a", `"needs":["b"],"writes":["same"]`),
		nodeJSON("b", `"needs":["a"],"writes":["same"]`),
	)
	_, err = Parse(jsontext.Value(cycleAndOverlap))
	if err == nil || err.Error() != "graph: dependency cycle" {
		t.Fatalf("cycle must precede overlap, got %v", err)
	}

	unknownBeforeCycle := graphJSON(
		nodeJSON("a", `"needs":["ghost","b"]`),
		nodeJSON("b", `"needs":["a"]`),
	)
	_, err = Parse(jsontext.Value(unknownBeforeCycle))
	if err == nil || err.Error() != "graph: node \"a\" has unknown need \"ghost\"" {
		t.Fatalf("unknown need must precede cycle, got %v", err)
	}
}
