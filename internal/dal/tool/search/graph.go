package search

import (
	"bytes"
	"cmp"
	"context"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/syntax"
)

type graphCall struct {
	entries []fileEntry
	args    args
	start   time.Time
	now     func() time.Time
}

type site struct {
	path string
	ref  syntax.Ref
	def  *syntax.Symbol
}

type importRow struct {
	path string
	line int
	spec string
}

type graphFile struct {
	path   string
	graph  syntax.Graph
	report syntax.Report
}

type graphView struct {
	call        graphCall
	eligible    []fileEntry
	files       map[string]graphFile
	attempted   map[string]bool
	parsedFiles int
	parsedBytes int64
	tally       callTally
}

func graphMode(ctx context.Context, call graphCall) (outcome, error) {
	ctx, cancel := context.WithTimeout(ctx, parseDeadline)
	defer cancel()
	files, err := parseCandidates(ctx, call.entries, call.args.Glob, supportedPath)
	if err != nil {
		return outcome{}, err
	}
	call.entries = files
	view := newGraphView(call)
	if call.args.Mode == "importers" {
		return view.importers(ctx)
	}
	return view.references(ctx)
}

func newGraphView(call graphCall) *graphView {
	v := &graphView{call: call, files: make(map[string]graphFile), attempted: make(map[string]bool)}
	for _, entry := range call.entries {
		lang := languageOf(entry.display)
		if lang.HasGraph() {
			v.eligible = append(v.eligible, entry)
		} else {
			v.tally.noteNoGraph(lang.Name())
		}
	}
	slices.SortFunc(v.eligible, func(a, b fileEntry) int {
		return cmp.Compare(a.display, b.display)
	})
	return v
}

func (v *graphView) load(ctx context.Context, needles []string) error {
	jobs, err := v.admit(ctx, needles)
	if err != nil {
		return err
	}
	parsed, err := graphJobs(ctx, jobs)
	if err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("search: %w", err)
	}
	reports := make([]syntax.Report, len(parsed))
	for i, file := range parsed {
		v.files[file.path] = file
		reports[i] = file.report
	}
	noteParses(jobs, reports, &v.tally)
	return nil
}

func (v *graphView) admit(ctx context.Context, needles []string) ([]parseJob, error) {
	var jobs []parseJob
	for i, file := range v.eligible {
		if v.attempted[file.display] {
			continue
		}
		if err := ctx.Err(); err != nil {
			return nil, fmt.Errorf("search: %w", err)
		}
		if v.call.now().Sub(v.call.start) >= parseBudget {
			v.refuseFrom(i)
			break
		}
		if job, ok := v.admitFile(file, needles); ok {
			jobs = append(jobs, job)
		}
	}
	return jobs, nil
}

func (v *graphView) refuseFrom(index int) {
	for _, file := range v.eligible[index:] {
		if !v.attempted[file.display] {
			v.attempted[file.display] = true
			v.tally.stopped++
		}
	}
}

func (v *graphView) admitFile(file fileEntry, needles []string) (parseJob, bool) {
	if !searchableFile(file.absolute) {
		v.attempted[file.display] = true
		return parseJob{}, false
	}
	data, ok := readSearchable(file.absolute)
	if !ok {
		v.attempted[file.display] = true
		return parseJob{}, false
	}
	if !containsAny(data, needles) {
		return parseJob{}, false
	}
	v.attempted[file.display] = true
	if v.refuses(len(data)) {
		v.tally.notParsed++
		return parseJob{}, false
	}
	v.parsedFiles++
	v.parsedBytes += int64(len(data))
	return parseJob{file: file, lang: languageOf(file.display), data: data}, true
}

func (v *graphView) refuses(size int) bool {
	return size > parseMaxFileBytes || v.parsedFiles >= parseMaxFiles || v.parsedBytes+int64(size) > parseMaxBytes
}

func containsAny(data []byte, needles []string) bool {
	if len(needles) == 0 {
		return true
	}
	return slices.ContainsFunc(needles, func(needle string) bool {
		return bytes.Contains(data, []byte(needle))
	})
}

func graphJobs(ctx context.Context, jobs []parseJob) ([]graphFile, error) {
	files, err := runJobs(jobs, func(parser *syntax.Parser, data []byte) (graphFile, error) {
		graph, report, err := parser.Graph(ctx, data)
		return graphFile{graph: graph, report: report}, err
	})
	if err != nil {
		return nil, err
	}
	for i := range files {
		files[i].path = jobs[i].file.display
	}
	return files, nil
}

func (v *graphView) callSites(names []string, kinds ...syntax.RefKind) []site {
	var out []site
	for _, entry := range v.eligible {
		file, ok := v.files[entry.display]
		if !ok {
			continue
		}
		for _, ref := range file.graph.Refs {
			if !slices.Contains(names, ref.Name) || !slices.Contains(kinds, ref.Kind) {
				continue
			}
			out = append(out, site{path: file.path, ref: ref, def: scopeSymbol(file.graph.Symbols, ref.Scope)})
		}
	}
	return out
}

func scopeSymbol(symbols []syntax.Symbol, scope int) *syntax.Symbol {
	if scope < 0 {
		return nil
	}
	return &symbols[scope]
}

func (v *graphView) importRows(spec string) []importRow {
	var out []importRow
	for _, entry := range v.eligible {
		file, ok := v.files[entry.display]
		if !ok {
			continue
		}
		for _, item := range file.graph.Imports {
			if importMatches(item.Spec, spec) {
				out = append(out, importRow{path: file.path, line: item.Line, spec: item.Spec})
			}
		}
	}
	return out
}

func importMatches(spec, pattern string) bool {
	if spec == pattern {
		return true
	}
	head, ok := strings.CutSuffix(spec, pattern)
	if !ok || head == "" {
		return false
	}
	switch head[len(head)-1] {
	case '/', '.', ':':
		return true
	}
	return false
}

func (v *graphView) references(ctx context.Context) (outcome, error) {
	name := v.call.args.Pattern
	if err := v.load(ctx, []string{name}); err != nil {
		return outcome{}, err
	}
	sites := v.callSites([]string{name}, syntax.RefCall, syntax.RefType)
	rows := make([]string, len(sites))
	for i, found := range sites {
		rows[i] = referenceRow(found)
	}
	return v.reply(rows), nil
}

func (v *graphView) importers(ctx context.Context) (outcome, error) {
	if err := v.load(ctx, nil); err != nil {
		return outcome{}, err
	}
	found := v.importRows(v.call.args.Pattern)
	rows := make([]string, len(found))
	for i, row := range found {
		rows[i] = fmt.Sprintf("%s:%d: import %s by name", row.path, row.line, row.spec)
	}
	return v.reply(rows), nil
}

func referenceRow(found site) string {
	where := "at top level"
	if found.def != nil {
		where = "in " + found.def.Qualified()
	}
	return fmt.Sprintf("%s:%d: %s %s %s by name", found.path, found.ref.Line, found.ref.Kind, found.ref.Name, where)
}

func (v *graphView) reply(rows []string) outcome {
	kept := min(len(rows), v.call.args.Limit)
	shown := rows[:kept]
	if len(shown) == 0 {
		shown = []string{fmt.Sprintf("No matches for %s by name. Searched %d files of languages with a graph.", v.call.args.Pattern, len(v.eligible))}
	}
	return outcome{Text: finishReply(shown, v.tally, len(rows)-kept, "rows")}
}
