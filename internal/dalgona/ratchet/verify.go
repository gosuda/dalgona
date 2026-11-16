package ratchet

import (
	"fmt"
	"maps"
	"slices"
	"strings"
)

func checkSlots(s *state, variant string, data variantData) ([]string, error) {
	results := resultCounts(data.Rows)
	if dups := duplicateSlots(results); len(dups) > 0 {
		return nil, fmt.Errorf("%s/results.jsonl has duplicate (case, rep) rows (%s); keep one row per slot and gate again", variant, listed(dups))
	}
	missing, erroredOnly := classifySlots(s, results, data.Errors)
	if len(missing) > 0 {
		return nil, fmt.Errorf("%s is incomplete: %d (case, rep) slots have no result or error row (%s)", variant, len(missing), listed(missing))
	}
	if variant == "baseline" && len(erroredOnly) > 0 {
		return nil, fmt.Errorf("baseline has %d slots that only errored (%s); fix the harness and resume the run until every slot is scored", len(erroredOnly), listed(erroredOnly))
	}
	return erroredOnly, nil
}

func resultCounts(rows []resultRow) map[string]int {
	counts := make(map[string]int, len(rows))
	for _, row := range rows {
		counts[slot(row.CaseID, row.Rep)]++
	}
	return counts
}

func duplicateSlots(counts map[string]int) []string {
	var dups []string
	for key, n := range counts {
		if n > 1 {
			dups = append(dups, key)
		}
	}
	slices.Sort(dups)
	return dups
}

func classifySlots(s *state, results map[string]int, errs []errorRow) (missing, erroredOnly []string) {
	errored := make(map[string]bool, len(errs))
	for _, e := range errs {
		errored[slot(e.CaseID, e.Rep)] = true
	}
	for _, id := range slices.Concat(s.TrainIDs, s.TestIDs) {
		for rep := range s.Reps {
			key := slot(id, rep)
			switch {
			case results[key] > 0:
			case errored[key]:
				erroredOnly = append(erroredOnly, key)
			default:
				missing = append(missing, key)
			}
		}
	}
	return missing, erroredOnly
}

func slot(id string, rep int) string {
	return fmt.Sprintf("%s#%d", id, rep)
}

func checkLeaks(s *state, variant string, data variantData) error {
	if err := checkTraces(s, variant, data); err != nil {
		return err
	}
	return checkTestText(s, data.Rows)
}

func checkTraces(s *state, variant string, data variantData) error {
	test := idSet(s.TestIDs)
	var leaked []string
	for _, name := range data.Traces {
		m := traceName.FindStringSubmatch(name)
		switch {
		case m == nil:
			return fmt.Errorf("%s/traces holds a file that is not <case>_rep<k>.json: %s", variant, name)
		case test[m[1]]:
			leaked = append(leaked, name)
		}
	}
	if len(leaked) > 0 {
		return fmt.Errorf("%s/traces holds transcripts for test cases (%s); write traces for train cases only, delete these, and run again", variant, strings.Join(leaked, ", "))
	}
	return nil
}

func checkTestText(s *state, rows []resultRow) error {
	test := idSet(s.TestIDs)
	for _, row := range rows {
		if !test[row.CaseID] {
			continue
		}
		if key, ok := textField(row); ok {
			return fmt.Errorf("test row %s carries text field %s; keep output text of test cases out of results.jsonl", slot(row.CaseID, row.Rep), key)
		}
	}
	return nil
}

func textField(row resultRow) (string, bool) {
	for _, key := range slices.Sorted(maps.Keys(row.Fields)) {
		if key == "case_id" || key == "status" || key == "model" {
			continue
		}
		if row.Fields[key].Kind() == '"' {
			return key, true
		}
	}
	return "", false
}

func dataWarnings(data variantData, known map[string]bool) []string {
	strays := make(map[string]bool)
	for _, row := range data.Rows {
		if !known[row.CaseID] {
			strays[row.CaseID] = true
		}
	}
	var warnings []string
	if n := len(strays); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d result ids are outside the split and were ignored", n))
	}
	if n := countTruncated(filterRows(data.Rows, known)); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d truncated rows excluded from quality means", n))
	}
	if n := len(data.Errors); n > 0 {
		warnings = append(warnings, fmt.Sprintf("%d infra error rows recorded in errors.jsonl", n))
	}
	return warnings
}

func countTruncated(rows []resultRow) int {
	n := 0
	for _, row := range rows {
		if row.Status == "truncated" {
			n++
		}
	}
	return n
}
