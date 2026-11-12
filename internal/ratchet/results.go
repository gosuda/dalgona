package ratchet

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

const (
	maxRowBytes     = 1 << 20
	maxResultsBytes = 256 << 20
)

var traceName = regexp.MustCompile(`^(.+)_rep[0-9]+\.json$`)

type resultRow struct {
	CaseID  string
	Rep     int
	Status  string
	Raw     jsontext.Value
	Fields  map[string]jsontext.Value
	Metrics map[string]float64
}

type errorRow struct {
	CaseID string
	Rep    int
}

type variantData struct {
	Rows       []resultRow
	Errors     []errorRow
	Traces     []string
	ResultsSHA string
}

func (f *flow) readVariant(variant string) (variantData, error) {
	dir := filepath.Join(f.dir, variant)
	results, found, err := f.readFile(filepath.Join(dir, "results.jsonl"))
	if err != nil {
		return variantData{}, err
	}
	if !found {
		return variantData{}, fmt.Errorf("no results for %s: %s/results.jsonl is missing", variant, variant)
	}
	errs, _, err := f.readFile(filepath.Join(dir, "errors.jsonl"))
	if err != nil {
		return variantData{}, err
	}
	traces, err := f.traceNames(filepath.Join(dir, "traces"))
	if err != nil {
		return variantData{}, err
	}
	rows, err := parseRows(variant+"/results.jsonl", results)
	if err != nil {
		return variantData{}, err
	}
	errorRows, err := parseErrors(variant+"/errors.jsonl", errs)
	if err != nil {
		return variantData{}, err
	}
	return variantData{Rows: rows, Errors: errorRows, Traces: traces, ResultsSHA: resultsSum(results, errs, traces)}, nil
}

func (f *flow) readFile(rel string) ([]byte, bool, error) {
	file, err := f.root.Open(filepath.FromSlash(rel))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, fmt.Errorf("open %s: %w", rel, err)
	}
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(io.LimitReader(file, maxResultsBytes+1))
	if err != nil {
		return nil, false, fmt.Errorf("read %s: %w", rel, err)
	}
	if len(data) > maxResultsBytes {
		return nil, false, fmt.Errorf("%s is larger than %d bytes", rel, maxResultsBytes)
	}
	return data, true, nil
}

func (f *flow) traceNames(rel string) ([]string, error) {
	dir, err := f.root.Open(filepath.FromSlash(rel))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", rel, err)
	}
	defer func() { _ = dir.Close() }()
	entries, err := dir.ReadDir(-1)
	if err != nil {
		return nil, fmt.Errorf("list %s: %w", rel, err)
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	slices.Sort(names)
	return names, nil
}

func resultsSum(results, errs []byte, traces []string) string {
	hasher := sha256.New()
	_, _ = hasher.Write([]byte("results\x00"))
	_, _ = hasher.Write(results)
	_, _ = hasher.Write([]byte("\x00errors\x00"))
	_, _ = hasher.Write(errs)
	_, _ = hasher.Write([]byte("\x00traces\x00"))
	_, _ = hasher.Write([]byte(strings.Join(traces, "\n")))
	return hex.EncodeToString(hasher.Sum(nil))
}

func eachLine(data []byte, visit func(number int, line []byte) error) error {
	scanner := bufio.NewScanner(bytes.NewReader(data))
	scanner.Buffer(make([]byte, 0, 64<<10), maxRowBytes)
	for number := 1; scanner.Scan(); number++ {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		if err := visit(number, line); err != nil {
			return err
		}
	}
	return scanner.Err()
}

func parseRows(label string, data []byte) ([]resultRow, error) {
	var rows []resultRow
	err := eachLine(data, func(number int, line []byte) error {
		row, err := decodeRow(line)
		if err != nil {
			return fmt.Errorf("%s line %d: %w", label, number, err)
		}
		rows = append(rows, row)
		return nil
	})
	return rows, err
}

func parseErrors(label string, data []byte) ([]errorRow, error) {
	var rows []errorRow
	err := eachLine(data, func(number int, line []byte) error {
		row, err := decodeErrorRow(line)
		if err != nil {
			return fmt.Errorf("%s line %d: %w", label, number, err)
		}
		rows = append(rows, row)
		return nil
	})
	return rows, err
}

func decodeRow(line []byte) (resultRow, error) {
	var fields map[string]jsontext.Value
	if err := json.Unmarshal(line, &fields); err != nil {
		return resultRow{}, fmt.Errorf("malformed JSON: %w", err)
	}
	caseID, err := stringField(fields, "case_id")
	if err != nil {
		return resultRow{}, err
	}
	rep, err := repField(fields)
	if err != nil {
		return resultRow{}, err
	}
	status, err := statusField(fields)
	if err != nil {
		return resultRow{}, err
	}
	metrics := numberMetrics(fields)
	if status != "truncated" {
		if err = addGrade(metrics, fields); err != nil {
			return resultRow{}, err
		}
	}
	return resultRow{CaseID: caseID, Rep: rep, Status: status, Raw: bytes.Clone(line), Fields: fields, Metrics: metrics}, nil
}

func decodeErrorRow(line []byte) (errorRow, error) {
	var fields map[string]jsontext.Value
	if err := json.Unmarshal(line, &fields); err != nil {
		return errorRow{}, fmt.Errorf("malformed JSON: %w", err)
	}
	caseID, err := stringField(fields, "case_id")
	if err != nil {
		return errorRow{}, err
	}
	rep, err := repField(fields)
	if err != nil {
		return errorRow{}, err
	}
	if _, err := stringField(fields, "class"); err != nil {
		return errorRow{}, err
	}
	return errorRow{CaseID: caseID, Rep: rep}, nil
}

func stringField(fields map[string]jsontext.Value, key string) (string, error) {
	value, ok := fields[key]
	if !ok || value.Kind() != '"' {
		return "", fmt.Errorf("%s must be a non-empty string", key)
	}
	var text string
	if err := json.Unmarshal(value, &text); err != nil || text == "" {
		return "", fmt.Errorf("%s must be a non-empty string", key)
	}
	return text, nil
}

func repField(fields map[string]jsontext.Value) (int, error) {
	n, ok := integerValue(fields["rep"])
	if !ok || n < 0 {
		return 0, errors.New("rep must be a non-negative integer")
	}
	return n, nil
}

func integerValue(value jsontext.Value) (int, bool) {
	if value.Kind() != '0' {
		return 0, false
	}
	n, err := strconv.Atoi(string(value))
	return n, err == nil
}

func statusField(fields map[string]jsontext.Value) (string, error) {
	value, ok := fields["status"]
	if !ok {
		return "", errors.New("status is missing; it must be ok or truncated")
	}
	var status string
	if value.Kind() == '"' && json.Unmarshal(value, &status) == nil && (status == "ok" || status == "truncated") {
		return status, nil
	}
	return "", errors.New("status must be ok or truncated")
}

func scalarValue(value jsontext.Value) (float64, bool) {
	kind := value.Kind()
	if kind == 't' {
		return 1, true
	}
	if kind == 'f' {
		return 0, true
	}
	if kind != '0' {
		return 0, false
	}
	n, err := strconv.ParseFloat(string(value), 64)
	return n, err == nil
}

const gradeShape = "grade must be a number, a boolean, or an object of numbers and booleans"

func addGrade(metrics map[string]float64, fields map[string]jsontext.Value) error {
	grade, ok := fields["grade"]
	if !ok {
		return errors.New("grade is missing; it must be a number, a boolean, or an object of numbers and booleans")
	}
	if grade.Kind() != '{' {
		n, ok := scalarValue(grade)
		if !ok {
			return errors.New(gradeShape)
		}
		metrics["score"] = n
		return nil
	}
	members, err := gradeObject(grade)
	if err != nil {
		return err
	}
	maps.Copy(metrics, members)
	return nil
}

func numberMetrics(fields map[string]jsontext.Value) map[string]float64 {
	metrics := make(map[string]float64)
	for key, value := range fields {
		if n, ok := scalarValue(value); ok && key != "rep" && value.Kind() == '0' {
			metrics[key] = n
		}
	}
	return metrics
}

func gradeObject(grade jsontext.Value) (map[string]float64, error) {
	var members map[string]jsontext.Value
	if err := json.Unmarshal(grade, &members); err != nil {
		return nil, errors.New(gradeShape)
	}
	out := make(map[string]float64, len(members))
	for key, value := range members {
		n, ok := scalarValue(value)
		if !ok {
			return nil, fmt.Errorf("grade member %s must be a number or a boolean", key)
		}
		out[key] = n
	}
	return out, nil
}

type caseMeans map[string]float64

func meansByCase(rows []resultRow, metric string) caseMeans {
	sums := make(map[string]float64)
	counts := make(map[string]int)
	for _, row := range rows {
		value, ok := row.Metrics[metric]
		if !ok {
			continue
		}
		sums[row.CaseID] += value
		counts[row.CaseID]++
	}
	means := make(caseMeans, len(counts))
	for id, n := range counts {
		means[id] = sums[id] / float64(n)
	}
	return means
}

func summarize(means caseMeans, ids []string) summary {
	var values []float64
	for _, id := range ids {
		if value, ok := means[id]; ok {
			values = append(values, value)
		}
	}
	return meanAndHalf(values)
}

func filterRows(rows []resultRow, known map[string]bool) []resultRow {
	var kept []resultRow
	for _, row := range rows {
		if known[row.CaseID] {
			kept = append(kept, row)
		}
	}
	return kept
}

func idSet(ids []string) map[string]bool {
	set := make(map[string]bool, len(ids))
	for _, id := range ids {
		set[id] = true
	}
	return set
}
