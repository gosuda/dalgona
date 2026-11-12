package rpc

import (
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

var dalgonaNotifications = []string{
	"dalgona/thread/childSpawned",
	"dalgona/advisor/note",
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
