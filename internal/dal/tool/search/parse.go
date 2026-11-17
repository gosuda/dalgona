package search

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"maps"
	"regexp"
	regexpsyntax "regexp/syntax"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/syntax"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const (
	parseWorkers      = 8
	parseMaxFiles     = 256
	parseMaxFileBytes = 256 << 10
	parseMaxBytes     = 1 << 20
	parseBudget       = 10 * time.Second
	parseDeadline     = 30 * time.Second
	anchorRefusal     = `search: name patterns cannot use ^, $, \A, \z, \b or \B; match the name only`
)

var (
	errParseDeadline = errors.New("search: parse deadline reached")
	parseWorkerCount = parseWorkers
	symbolKinds      = []string{"function", "method", "class", "interface", "type", "enum", "constant", "variable", "module", "macro", "section"}
)

type callTally struct {
	parseNotes   []string
	notParsed    int
	syntaxOnly   int
	noGraph      int
	noGraphLangs map[string]bool
	stopped      int
}

func (c *callTally) noteNoGraph(lang string) {
	c.noGraph++
	if c.noGraphLangs == nil {
		c.noGraphLangs = make(map[string]bool)
	}
	c.noGraphLangs[lang] = true
}

func (c callTally) footers() []string {
	out := slices.Clone(c.parseNotes)
	if c.notParsed > 0 {
		out = append(out, fmt.Sprintf("[Not parsed: %d files. Narrow path or glob.]", c.notParsed))
	}
	if c.syntaxOnly > 0 {
		out = append(out, fmt.Sprintf("[Syntax only: %d files. Use mode syntax with lang.]", c.syntaxOnly))
	}
	if c.noGraph > 0 {
		langs := strings.Join(slices.Sorted(maps.Keys(c.noGraphLangs)), ", ")
		out = append(out, fmt.Sprintf("[No graph: %d files of %s. Use grep.]", c.noGraph, langs))
	}
	if c.stopped > 0 {
		out = append(out, fmt.Sprintf("[Stopped: %d files not parsed. Narrow path or glob.]", c.stopped))
	}
	return out
}

type parseJob struct {
	file   fileEntry
	lang   syntax.Language
	data   []byte
	starts []int
}

type parseResult struct {
	symbols []syntax.Symbol
	report  syntax.Report
}

func languageOf(display string) syntax.Language {
	lang, _ := syntax.ForPath(display)
	return lang
}

func supportedPath(display string) bool {
	_, ok := syntax.ForPath(display)
	return ok
}

func parseCandidates(ctx context.Context, entries []fileEntry, glob string, accept func(display string) bool) ([]fileEntry, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	kept := make([]fileEntry, 0, len(entries))
	for _, entry := range entries {
		if accept(entry.display) {
			kept = append(kept, entry)
		}
	}
	if glob == "" {
		return kept, nil
	}
	return matchingFiles(kept, glob)
}

func namePattern(args args) (nameMatcher, error) {
	var matcher nameMatcher
	if args.Pattern == "" {
		return matcher, nil
	}
	whole, err := compileNamePattern(args.Pattern, args)
	if err != nil {
		return matcher, err
	}
	matcher.whole = whole
	ownerRaw, nameRaw, split := splitOwnerPattern(args.Pattern)
	if !split {
		return matcher, nil
	}
	owner, err := compileNamePattern(ownerRaw, args)
	if err != nil {
		return matcher, err
	}
	name, err := compileNamePattern(nameRaw, args)
	if err != nil {
		return matcher, err
	}
	matcher.owner = owner
	matcher.name = name
	return matcher, nil
}

func compileNamePattern(raw string, args args) (*regexp.Regexp, error) {
	sub := args
	sub.Pattern = raw
	parsed, err := regexpsyntax.Parse(grepPattern(sub), regexpsyntax.Perl)
	if err != nil {
		return nil, fmt.Errorf("search: %w", err)
	}
	if hasAnchor(parsed) {
		return nil, errors.New(anchorRefusal)
	}
	return grepRegexp(sub)
}

var plainName = regexp.MustCompile(`^[A-Za-z0-9_$]+$`)

func splitOwnerPattern(raw string) (string, string, bool) {
	owner, tail, found := strings.CutLast(raw, ".")
	if !found || owner == "" || tail == "" {
		return "", "", false
	}
	owner, _ = strings.CutSuffix(owner, `\`)
	if owner == "" || !plainName.MatchString(tail) {
		return "", "", false
	}
	return owner, tail, true
}

type nameMatcher struct {
	whole *regexp.Regexp
	owner *regexp.Regexp
	name  *regexp.Regexp
}

func (m nameMatcher) empty() bool {
	return m.whole == nil && m.owner == nil && m.name == nil
}

func (m nameMatcher) matchesSymbol(s syntax.Symbol) bool {
	if m.empty() {
		return true
	}
	if m.whole != nil && m.whole.MatchString(s.Name) {
		return true
	}
	return m.owner != nil && m.name != nil && s.Owner != "" &&
		m.owner.MatchString(s.Owner) && m.name.MatchString(s.Name)
}

func (m nameMatcher) selects(data []byte) bool {
	if m.whole == nil && m.name == nil {
		return true
	}
	starts := splitStarts(data)
	for i := range starts {
		line := lineBytes(data, starts, i+1)
		if m.whole != nil && m.whole.Match(line) {
			return true
		}
		if m.name != nil && m.name.Match(line) {
			return true
		}
	}
	return false
}

func isAnchor(op regexpsyntax.Op) bool {
	return op == regexpsyntax.OpBeginLine || op == regexpsyntax.OpEndLine ||
		op == regexpsyntax.OpBeginText || op == regexpsyntax.OpEndText ||
		op == regexpsyntax.OpWordBoundary || op == regexpsyntax.OpNoWordBoundary
}

func hasAnchor(re *regexpsyntax.Regexp) bool {
	return isAnchor(re.Op) || slices.ContainsFunc(re.Sub, hasAnchor)
}

type parseRun struct {
	matcher  nameMatcher
	jobs     []parseJob
	results  []parseResult
	searched int
	tally    callTally
}

func parseFiles(ctx context.Context, entries []fileEntry, args args, start time.Time, now func() time.Time) (parseRun, error) {
	var run parseRun
	matcher, err := namePattern(args)
	if err != nil {
		return run, err
	}
	run.matcher = matcher
	candidates, err := parseCandidates(ctx, entries, args.Glob, supportedPath)
	if err != nil {
		return run, err
	}
	files := make([]fileEntry, 0, len(candidates))
	for _, entry := range candidates {
		if languageOf(entry.display).HasDefinitions() {
			files = append(files, entry)
		} else {
			run.tally.syntaxOnly++
		}
	}
	run.searched = len(files)
	run.jobs = dispatchSymbols(files, matcher, start, now, &run.tally)
	run.results, err = parseJobs(ctx, run.jobs)
	if err != nil {
		return run, err
	}
	reports := make([]syntax.Report, len(run.results))
	for i, result := range run.results {
		reports[i] = result.report
	}
	noteParses(run.jobs, reports, &run.tally)
	return run, nil
}

func noteParses(jobs []parseJob, reports []syntax.Report, tally *callTally) {
	for i, job := range jobs {
		switch {
		case reports[i].Stopped:
			tally.parseNotes = append(tally.parseNotes, "[Parse: stopped, no rows: "+job.file.display+"]")
		case reports[i].SyntaxErrors:
			tally.parseNotes = append(tally.parseNotes, "[Parse: syntax errors, rows kept: "+job.file.display+"]")
		}
	}
}

func symbolsMode(ctx context.Context, entries []fileEntry, args args, start time.Time, now func() time.Time) (outcome, error) {
	run, err := parseFiles(ctx, entries, args, start, now)
	if err != nil {
		return outcome{}, err
	}
	rows, truncated := symbolRows(run.jobs, run.results, args, run.matcher)
	if len(rows) == 0 {
		rows = []string{emptySymbolsReply(args.Pattern, run.searched)}
	}
	return outcome{Text: finishReply(rows, run.tally, truncated, "symbols")}, nil
}

func finishReply(lines []string, tally callTally, cut int, unit string) string {
	lines = append(lines, tally.footers()...)
	if cut > 0 {
		lines = append(lines, fmt.Sprintf("[Truncated: %d more %s]", cut, unit))
	}
	return strings.Join(lines, "\n")
}

func definitionMode(ctx context.Context, entries []fileEntry, args args, start time.Time, now func() time.Time) (outcome, error) {
	run, err := parseFiles(ctx, entries, args, start, now)
	if err != nil {
		return outcome{}, err
	}
	rows, captures, cut := definitionRows(run, args)
	if len(rows) == 0 {
		rows = []string{emptySymbolsReply(args.Pattern, run.searched)}
	}
	return outcome{Text: finishReply(rows, run.tally, cut, "lines"), Captures: captures}, nil
}

type definitionTarget struct {
	job    int
	symbol syntax.Symbol
	last   int
}

func wantedDefinitions(run parseRun, args args) ([]definitionTarget, int) {
	var targets []definitionTarget
	total := 0
	for i, result := range run.results {
		lastLine := len(run.jobs[i].starts)
		for _, symbol := range result.symbols {
			end := min(symbol.End, lastLine)
			if !symbolWanted(symbol, args, run.matcher) || end < symbol.Start {
				continue
			}
			targets = append(targets, definitionTarget{job: i, symbol: symbol, last: end})
			total += end - symbol.Start + 1
		}
	}
	return targets, total
}

func definitionRows(run parseRun, args args) ([]string, []capture, int) {
	targets, total := wantedDefinitions(run, args)
	var rows []string
	files := make([]capture, len(run.jobs))
	printed := 0
	for _, target := range targets {
		if printed == args.Limit {
			break
		}
		job := run.jobs[target.job]
		last := min(target.last, target.symbol.Start+args.Limit-printed-1)
		rows = append(rows, symbolRow(job, target.symbol))
		rows, files[target.job].Shown = appendBody(rows, job, target.symbol.Start, last, files[target.job].Shown)
		files[target.job].Path = job.file.display
		files[target.job].Data = job.data
		printed += last - target.symbol.Start + 1
	}
	return rows, printedCaptures(files), total - printed
}

func printedCaptures(files []capture) []capture {
	var out []capture
	for _, file := range files {
		if file.Path != "" {
			out = append(out, file)
		}
	}
	return out
}

func appendBody(rows []string, job parseJob, from, to int, shown []snapshot.Range) ([]string, []snapshot.Range) {
	for line := from; line <= to; line++ {
		text, complete := cropMatch(string(lineBytes(job.data, job.starts, line)))
		rows = append(rows, fmt.Sprintf("%s:%d: %s", job.file.display, line, text))
		if complete {
			shown = appendShown(shown, line)
		}
	}
	return rows, shown
}

func emptySymbolsReply(pattern string, searched int) string {
	if pattern == "" {
		return fmt.Sprintf("No symbols. Searched %d files of supported languages.", searched)
	}
	return fmt.Sprintf("No matches for %s. Searched %d files of supported languages.", pattern, searched)
}

func dispatchSymbols(files []fileEntry, matcher nameMatcher, start time.Time, now func() time.Time, tally *callTally) []parseJob {
	var jobs []parseJob
	total := 0
	for i, file := range files {
		if now().Sub(start) >= parseBudget {
			tally.stopped = len(files) - i
			break
		}
		data, ok := candidateData(file, matcher)
		if !ok {
			continue
		}
		switch {
		case len(data) > parseMaxFileBytes, len(jobs) >= parseMaxFiles, total+len(data) > parseMaxBytes:
			tally.notParsed++
		default:
			total += len(data)
			jobs = append(jobs, parseJob{file: file, lang: languageOf(file.display), data: data, starts: splitStarts(data)})
		}
	}
	return jobs
}

func candidateData(file fileEntry, matcher nameMatcher) ([]byte, bool) {
	if matcher.empty() {
		if !searchableFile(file.absolute) {
			return nil, false
		}
		return readSearchable(file.absolute)
	}
	data, ok := readSearchable(file.absolute)
	if !ok {
		return nil, false
	}
	return data, matcher.selects(data)
}

func runJobs[T any](jobs []parseJob, parse func(*syntax.Parser, []byte) (T, error)) ([]T, error) {
	results := make([]T, len(jobs))
	errs := make([]error, len(jobs))
	pending := make(chan int, len(jobs))
	for i := range jobs {
		pending <- i
	}
	close(pending)
	var wg sync.WaitGroup
	for range min(parseWorkerCount, len(jobs)) {
		wg.Go(func() {
			parsers := make(map[string]*syntax.Parser)
			for i := range pending {
				job := jobs[i]
				parser, ok := parsers[job.lang.Name()]
				if !ok {
					parser = job.lang.NewParser()
					parsers[job.lang.Name()] = parser
				}
				results[i], errs[i] = parse(parser, job.data)
			}
		})
	}
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return nil, err
		}
	}
	return results, nil
}

func parseJobs(ctx context.Context, jobs []parseJob) ([]parseResult, error) {
	return runJobs(jobs, func(parser *syntax.Parser, data []byte) (parseResult, error) {
		symbols, report, err := parser.Symbols(ctx, data)
		return parseResult{symbols: symbols, report: report}, err
	})
}

type captureResult struct {
	captures []syntax.Capture
	report   syntax.Report
}

func captureJobs(ctx context.Context, jobs []parseJob, query *syntax.Query) ([]captureResult, error) {
	return runJobs(jobs, func(parser *syntax.Parser, data []byte) (captureResult, error) {
		captures, report, err := parser.Captures(ctx, query, data)
		return captureResult{captures: captures, report: report}, err
	})
}

func syntaxMode(ctx context.Context, entries []fileEntry, args args, start time.Time, now func() time.Time) (outcome, error) {
	lang, ok := syntax.ByName(args.Lang)
	if !ok {
		return outcome{}, fmt.Errorf("search: unknown lang %s", args.Lang)
	}
	query, err := lang.Compile(args.Pattern)
	if err != nil {
		return outcome{}, err
	}
	candidates, err := parseCandidates(ctx, entries, args.Glob, func(display string) bool {
		found, ok := syntax.ForPath(display)
		return ok && found.Name() == lang.Name()
	})
	if err != nil {
		return outcome{}, err
	}
	var tally callTally
	jobs := dispatchSymbols(candidates, nameMatcher{}, start, now, &tally)
	results, err := captureJobs(ctx, jobs, query)
	if err != nil {
		return outcome{}, err
	}
	reports := make([]syntax.Report, len(results))
	for i, result := range results {
		reports[i] = result.report
	}
	noteParses(jobs, reports, &tally)
	rows, truncated := captureRows(jobs, results, args.Limit)
	if len(rows) == 0 {
		rows = []string{emptySyntaxReply(args.Pattern, lang.Name(), len(candidates))}
	}
	return outcome{Text: finishReply(rows, tally, truncated, "captures")}, nil
}

func emptySyntaxReply(pattern, lang string, searched int) string {
	return fmt.Sprintf("No matches for %s. Searched %d files of language %s.", pattern, searched, lang)
}

func captureRows(jobs []parseJob, results []captureResult, limit int) ([]string, int) {
	var rows []string
	truncated := 0
	for i, job := range jobs {
		for _, capture := range sortedCaptures(results[i].captures) {
			if len(rows) < limit {
				rows = append(rows, captureRow(job, capture))
			} else {
				truncated++
			}
		}
	}
	return rows, truncated
}

func sortedCaptures(captures []syntax.Capture) []syntax.Capture {
	out := slices.Clone(captures)
	slices.SortStableFunc(out, func(a, b syntax.Capture) int {
		return cmp.Or(cmp.Compare(a.Line, b.Line), cmp.Compare(a.Column, b.Column))
	})
	return out
}

func captureRow(job parseJob, capture syntax.Capture) string {
	text := string(job.data[capture.StartByte:capture.EndByte])
	text, _, _ = strings.Cut(text, "\n")
	cut, _ := cropMatch(text)
	return fmt.Sprintf("%s:%d: %s: %s", job.file.display, capture.Line, capture.Name, cut)
}

func symbolRows(jobs []parseJob, results []parseResult, args args, matcher nameMatcher) ([]string, int) {
	var rows []string
	truncated := 0
	for i, job := range jobs {
		for _, symbol := range results[i].symbols {
			if !symbolWanted(symbol, args, matcher) {
				continue
			}
			if len(rows) < args.Limit {
				rows = append(rows, symbolRow(job, symbol))
			} else {
				truncated++
			}
		}
	}
	return rows, truncated
}

func symbolWanted(symbol syntax.Symbol, args args, matcher nameMatcher) bool {
	if len(args.Kind) > 0 && !slices.Contains(args.Kind, string(symbol.Kind)) {
		return false
	}
	return matcher.matchesSymbol(symbol)
}

func symbolRow(job parseJob, symbol syntax.Symbol) string {
	text, _ := cropMatch(strings.TrimLeft(firstLine(job.data, job.starts, symbol.Start), " \t"))
	return fmt.Sprintf("%s%s:%d-%d: %s %s", strings.Repeat("  ", symbol.Depth), job.file.display, symbol.Start, symbol.End, symbol.Kind, text)
}

func firstLine(data []byte, starts []int, line int) string {
	if line < 1 || line > len(starts) {
		return ""
	}
	return string(lineBytes(data, starts, line))
}
