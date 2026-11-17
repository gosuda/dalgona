package rpc

import (
	json "encoding/json/v2"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/santhosh-tekuri/jsonschema/v6"
)

var dalgonaNotifications = []string{
	"dalgona/thread/childSpawned",
	"dalgona/advisor/note",
	"dalgona/retry/wait",
	"dalgona/tool/judged",
	"dalgona/turn/judged",
	"dalgona/turn/effort",
	"dalgona/sandbox/chosen",
}

func dalgonaSchemaName(method, suffix string) string {
	return strings.ReplaceAll(method, "/", ".") + suffix
}

func TestDalgonaMessagesHaveSchemas(t *testing.T) {
	names := slices.Clone(dalgonaNotifications)
	for method := range methods {
		if strings.HasPrefix(method, "dalgona/") && !slices.Contains(names, method) {
			names = append(names, method)
		}
	}
	slices.Sort(names)
	compiler := jsonschema.NewCompiler()
	for _, method := range names {
		files := []string{dalgonaSchemaName(method, ".params.json"), dalgonaSchemaName(method, ".result.json")}
		if slices.Contains(dalgonaNotifications, method) {
			files = []string{dalgonaSchemaName(method, ".json")}
		}
		for _, file := range files {
			if _, err := compiler.Compile(filepath.Join("testdata", "dalgona-schema", file)); err != nil {
				t.Errorf("%s has no usable schema: %v", method, err)
			}
		}
	}
}

func validateDalgona(t *testing.T, path string, raw []byte) error {
	t.Helper()
	sch, err := jsonschema.NewCompiler().Compile(filepath.Join("testdata", "dalgona-schema", path))
	if err != nil {
		t.Fatalf("compile %s: %v", path, err)
	}
	value, err := jsonschema.UnmarshalJSON(strings.NewReader(string(raw)))
	if err != nil {
		t.Fatalf("decode %s: %v", raw, err)
	}
	return sch.Validate(value)
}

func mutateDalgona(t *testing.T, schema string, raw []byte, member string) {
	t.Helper()
	decode := func(edit func(map[string]any)) []byte {
		var value map[string]any
		if err := json.Unmarshal(raw, &value); err != nil {
			t.Fatalf("decode %s: %v", raw, err)
		}
		if value == nil {
			value = make(map[string]any)
		}
		edit(value)
		encoded, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		return encoded
	}
	if err := validateDalgona(t, schema, decode(func(v map[string]any) { delete(v, member) })); err == nil {
		t.Errorf("%s without %s validates, want rejection", schema, member)
	}
	if err := validateDalgona(t, schema, decode(func(v map[string]any) { v[member] = 7 })); err == nil {
		t.Errorf("%s with %s mistyped validates, want rejection", schema, member)
	}
	if err := validateDalgona(t, schema, decode(func(v map[string]any) { v["xExtension"] = 7 })); err != nil {
		t.Errorf("%s rejects an unknown extension member: %v", schema, err)
	}
}

func TestDalgonaSchemaValidationRejectsBrokenMessages(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	response := c.call("dalgona/keywords/match", map[string]any{"text": "hello"})
	if response.Error != nil {
		t.Fatalf("dalgona/keywords/match failed: %d %s", response.Error.Code, response.Error.Message)
	}
	resultSchema := dalgonaSchemaName("dalgona/keywords/match", ".result.json")
	if err := validateDalgona(t, resultSchema, response.Result); err != nil {
		t.Fatalf("produced result %s does not validate: %v", response.Result, err)
	}
	mutateDalgona(t, resultSchema, response.Result, "matches")

	sr := newSandboxRig(t)
	sc := sr.connect()
	_, question := startSandboxTurn(t, sr, sc)
	sc.await("warning", nil)
	sc.respond(question.ID, `{"answers":{"q1":{"answers":["Accept"]}}}`)
	approval := awaitRequestNamed(t, sc, "item/commandExecution/requestApproval")
	sc.respond(approval.ID, `{"decision":"decline"}`)
	chosen := sc.await("dalgona/sandbox/chosen", nil)
	chosenSchema := dalgonaSchemaName("dalgona/sandbox/chosen", ".json")
	if err := validateDalgona(t, chosenSchema, chosen.Params); err != nil {
		t.Fatalf("produced notification %s does not validate: %v", chosen.Params, err)
	}
	mutateDalgona(t, chosenSchema, chosen.Params, "choice")
	sc.await("turn/completed", nil)
}
