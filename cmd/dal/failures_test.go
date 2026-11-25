package main

import (
	"bytes"
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/failure"
)

type passthroughMasker struct{}

func (passthroughMasker) MaskWithError(text string) (string, error) { return text, nil }

func captureDalFailure(t *testing.T, home string) failure.Record {
	t.Helper()
	store, err := failure.Open(t.Context(), failure.Options{Home: home, Product: "dal", Masker: passthroughMasker{}})
	if err != nil {
		t.Fatal(err)
	}
	record, err := store.Capture(t.Context(), failure.Input{
		Product: "dal", Class: failure.ClassTool, Tool: "exec", Err: errors.New("exec: start: fork failed"), Origin: failure.OriginMain,
	})
	if err != nil {
		t.Fatal(err)
	}
	return record
}

func TestFailuresListsAndPrintsTheReportText(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DAL_HOME", home)
	record := captureDalFailure(t, home)
	var stdout, stderr bytes.Buffer
	if code := run(t.Context(), []string{"failures"}, strings.NewReader(""), &stdout, &stderr); code != 0 {
		t.Fatalf("dal failures exit = %d, stderr %q", code, stderr.String())
	}
	if !strings.HasPrefix(stdout.String(), record.ID+"  ") || !strings.Contains(stdout.String(), "  tool  exec\n") {
		t.Fatalf("listing = %q, want the record id, class and tool", stdout.String())
	}
	stdout.Reset()
	if code := run(t.Context(), []string{"failures", record.ID}, strings.NewReader(""), &stdout, &stderr); code != 0 {
		t.Fatalf("dal failures <id> exit = %d, stderr %q", code, stderr.String())
	}
	if stdout.String() != failure.ReportText(record) {
		t.Fatalf("report = %q, want the shared report text", stdout.String())
	}
}

func TestFailuresRefusesUnknownAndMalformedIDs(t *testing.T) {
	t.Setenv("DAL_HOME", t.TempDir())
	for args, want := range map[[3]string]string{
		{"failures", strings.Repeat("a", 32)}:          "failure: record not found",
		{"failures", "x/../../../outside"}:             "failure id must be 32 lowercase hexadecimal digits",
		{"failures", strings.Repeat("A", 32)}:          "failure id must be 32 lowercase hexadecimal digits",
		{"failures", strings.Repeat("a", 32), "extra"}: failuresUsage,
	} {
		var stdout, stderr bytes.Buffer
		argv := args[:]
		if argv[2] == "" {
			argv = argv[:2]
		}
		code := run(t.Context(), argv, strings.NewReader(""), &stdout, &stderr)
		if code == 0 || stdout.Len() != 0 || !strings.Contains(stderr.String(), want) {
			t.Fatalf("%v exit = %d stdout %q stderr %q, want a refusal naming %q", argv, code, stdout.String(), stderr.String(), want)
		}
	}
}
