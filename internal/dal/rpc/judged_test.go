package rpc

import (
	"strings"
	"testing"
)

func TestJudgedSchemasRejectBrokenAttribution(t *testing.T) {
	valid := `{"threadId":"thr_1","turnId":"turn-3","itemId":"call_7","verdict":"clean","summary":"No problems found","paths":["a.go"],"by":"judge:a","fallback":"b: down","judges":[{"part":0,"by":"judge:a","fallback":"b: down"},{"part":1,"by":"judge:c"}]}`
	if err := validateDalgona(t, "dalgona.tool.judged.json", []byte(valid)); err != nil {
		t.Fatalf("valid attribution rejected: %v", err)
	}
	validTurn := strings.Replace(valid, `"itemId":"call_7",`, ``, 1)
	if err := validateDalgona(t, "dalgona.turn.judged.json", []byte(validTurn)); err != nil {
		t.Fatalf("valid turn attribution rejected: %v", err)
	}
	rejects := map[string]string{
		"no by":                 strings.Replace(valid, `"by":"judge:a","fallback":"b: down","judges"`, `"fallback":"b: down","judges"`, 1),
		"one judge entry":       strings.Replace(valid, `,{"part":1,"by":"judge:c"}`, ``, 1),
		"judge without part":    strings.Replace(valid, `{"part":1,"by":"judge:c"}`, `{"by":"judge:c"}`, 1),
		"negative part":         strings.Replace(valid, `"part":1`, `"part":-1`, 1),
		"fractional part":       strings.Replace(valid, `"part":1`, `"part":1.5`, 1),
		"judge without by":      strings.Replace(valid, `{"part":1,"by":"judge:c"}`, `{"part":1}`, 1),
		"judge extra member":    strings.Replace(valid, `{"part":1,"by":"judge:c"}`, `{"part":1,"by":"judge:c","x":1}`, 1),
		"fallback not a string": strings.Replace(valid, `"fallback":"b: down","judges"`, `"fallback":3,"judges"`, 1),
	}
	for name, raw := range rejects {
		if raw == valid {
			t.Fatalf("%s: the mutation did not change the payload", name)
		}
		if err := validateDalgona(t, "dalgona.tool.judged.json", []byte(raw)); err == nil {
			t.Errorf("%s: %s validates, want schema rejection", name, raw)
		}
		turn := strings.Replace(raw, `"itemId":"call_7",`, ``, 1)
		if err := validateDalgona(t, "dalgona.turn.judged.json", []byte(turn)); err == nil {
			t.Errorf("%s: turn payload %s validates, want schema rejection", name, turn)
		}
	}
}
