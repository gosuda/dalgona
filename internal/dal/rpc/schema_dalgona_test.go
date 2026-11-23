package rpc

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

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
