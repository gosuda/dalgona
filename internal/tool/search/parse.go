package search

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	regexpsyntax "regexp/syntax"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/syntax"
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
	parseNotes []string
	notParsed  int
	syntaxOnly int
	stopped    int
}

func (c callTally) footers() []string {
	out := slices.Clone(c.parseNotes)
	if c.notParsed > 0 {
		out = append(out, fmt.Sprintf("[Not parsed: %d files. Narrow path or glob.]", c.notParsed))
	}
	if c.syntaxOnly > 0 {
		out = append(out, fmt.Sprintf("[Syntax only: %d files. Use mode syntax with lang.]", c.syntaxOnly))
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
	kept := make([]fileEntry, 0, len(entries))
	for _, entry := range entries {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if accept(entry.display) {
			kept = append(kept, entry)
		}
	}
	if glob == "" {
		return kept, nil
	}
	return matchingFiles(kept, glob)
}

func namePattern(args args) (*regexp.Regexp, error) {
	if args.Pattern == "" {
		return nil, nil
	}
	parsed, err := regexpsyntax.Parse(grepPattern(args), regexpsyntax.Perl)
	if err != nil {
		return nil, fmt.Errorf("search: %w", err)
	}
	if hasAnchor(parsed) {
		return nil, errors.New(anchorRefusal)
	}
	return grepRegexp(args)
}

func isAnchor(op regexpsyntax.Op) bool {
	return op == regexpsyntax.OpBeginLine || op == regexpsyntax.OpEndLine ||
		op == regexpsyntax.OpBeginText || op == regexpsyntax.OpEndText ||
		op == regexpsyntax.OpWordBoundary || op == regexpsyntax.OpNoWordBoundary
}

func hasAnchor(re *regexpsyntax.Regexp) bool {
	if isAnchor(re.Op) {
		return true
	}
	for _, sub := range re.Sub {
		if hasAnchor(sub) {
			return true
		}
	}
	return false
}

func symbolsMode(ctx context.Context, entries []fileEntry, args args, start time.Time, now func() time.Time) (outcome, error) {
	matcher, err := namePattern(args)
	if err != nil {
		return outcome{}, err
	}
	candidates, err := parseCandidates(ctx, entries, args.Glob, supportedPath)
	if err != nil {
		return outcome{}, err
	}
	var tally callTally
	files := make([]fileEntry, 0, len(candidates))
	for _, entry := range candidates {
		if languageOf(entry.display).HasDefinitions() {
			files = append(files, entry)
		} else {
			tally.syntaxOnly++
		}
	}
	jobs := dispatchSymbols(files, matcher, start, now, &tally)
	results, err := parseJobs(ctx, jobs)
	if err != nil {
		return outcome{}, err
	}
	rows, truncated := symbolRows(jobs, results, args, matcher, &tally)
	lines := rows
	if len(rows) == 0 {
		lines = []string{emptySymbolsReply(args.Pattern, len(files))}
	}
	lines = append(lines, tally.footers()...)
	if truncated > 0 {
		lines = append(lines, fmt.Sprintf("[Truncated: %d more symbols]", truncated))
	}
	return outcome{Text: strings.Join(lines, "\n")}, nil
}

func emptySymbolsReply(pattern string, searched int) string {
	if pattern == "" {
		return fmt.Sprintf("No symbols. Searched %d files of supported languages.", searched)
	}
	return fmt.Sprintf("No matches for %s. Searched %d files of supported languages.", pattern, searched)
}

func dispatchSymbols(files []fileEntry, matcher *regexp.Regexp, start time.Time, now func() time.Time, tally *callTally) []parseJob {
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

func candidateData(file fileEntry, matcher *regexp.Regexp) ([]byte, bool) {
	if matcher == nil {
		if !searchableFile(file.absolute) {
			return nil, false
		}
		return readSearchable(file.absolute)
	}
	grepped, lines := scanFile(file, matcher)
	return grepped.data, len(lines) > 0
}

func parseJobs(ctx context.Context, jobs []parseJob) ([]parseResult, error) {
	results := make([]parseResult, len(jobs))
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
				results[i].symbols, results[i].report, errs[i] = parser.Symbols(ctx, job.data)
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

func symbolRows(jobs []parseJob, results []parseResult, args args, matcher *regexp.Regexp, tally *callTally) ([]string, int) {
	var rows []string
	truncated := 0
	for i, job := range jobs {
		result := results[i]
		if result.report.Stopped {
			tally.parseNotes = append(tally.parseNotes, "[Parse: stopped, no rows: "+job.file.display+"]")
			continue
		}
		if result.report.SyntaxErrors {
			tally.parseNotes = append(tally.parseNotes, "[Parse: syntax errors, rows kept: "+job.file.display+"]")
		}
		for _, symbol := range result.symbols {
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

func symbolWanted(symbol syntax.Symbol, args args, matcher *regexp.Regexp) bool {
	if len(args.Kind) > 0 && !slices.Contains(args.Kind, string(symbol.Kind)) {
		return false
	}
	return matcher == nil || matcher.MatchString(symbol.Name)
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
