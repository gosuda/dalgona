package search

import (
	"bytes"
	"cmp"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const maxSearchFileBytes = 16 << 20

type fileEntry struct {
	absolute  string
	display   string
	searchRel string
}

type hit struct {
	file int
	line int
}

type grepFile struct {
	entry  fileEntry
	data   []byte
	starts []int
}

func execute(cwd string, args args) (outcome, error) {
	scope, err := resolveScope(cwd, args.Path)
	if err != nil {
		return outcome{}, err
	}
	entries, err := walkScope(cwd, scope)
	if err != nil {
		return outcome{}, err
	}
	if args.Mode == "find" {
		return find(entries, args)
	}
	return grep(entries, args)
}

func resolveScope(cwd, raw string) (string, error) {
	if raw == "" {
		raw = "."
	}
	if filepath.IsAbs(raw) || outside(filepath.Clean(raw)) {
		return "", fmt.Errorf("search: path must stay within the workspace")
	}
	path := filepath.Join(cwd, filepath.Clean(raw))
	if _, err := os.Stat(path); err != nil {
		return "", fmt.Errorf("search: %s does not exist: %w", raw, err)
	}
	if err := checkScopeContainment(cwd, path, raw); err != nil {
		return "", err
	}
	return path, nil
}

func checkScopeContainment(cwd, path, raw string) error {
	root, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return fmt.Errorf("search: cannot inspect %s: %w", raw, err)
	}
	target, err := filepath.EvalSymlinks(path)
	if err != nil {
		return fmt.Errorf("search: cannot inspect %s: %w", raw, err)
	}
	if escapesRoot(root, target) {
		return fmt.Errorf("search: %s escapes the workspace via symlink", raw)
	}
	return nil
}

func escapesRoot(root, target string) bool {
	rel, err := filepath.Rel(root, target)
	if err != nil {
		return true
	}
	return rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

type walker struct {
	cwd     string
	scope   string
	root    string
	ignores ignoreStack
	entries []fileEntry
}

func walkScope(cwd, scope string) ([]fileEntry, error) {
	root, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return nil, fmt.Errorf("search: cannot inspect workspace: %w", err)
	}
	w := &walker{cwd: cwd, scope: scope, root: root, ignores: loadIgnore(cwd)}
	w.ignores.loadScopeIgnores(cwd, scope)
	if walkErr := filepath.WalkDir(scope, w.visit); walkErr != nil {
		return nil, fmt.Errorf("search: cannot walk %s: %w", scope, walkErr)
	}
	info, err := os.Stat(scope)
	if err == nil && !info.IsDir() {
		if !w.contained(scope) {
			return w.entries, nil
		}
		if addErr := w.add(scope); addErr != nil {
			return nil, addErr
		}
	}
	slices.SortFunc(w.entries, func(a, b fileEntry) int { return cmp.Compare(a.display, b.display) })
	return w.entries, nil
}

func (w *walker) visit(path string, entry fs.DirEntry, err error) error {
	if err != nil {
		return err
	}
	if path == w.scope {
		return nil
	}
	if w.excluded(path, entry) {
		return skipEntry(entry)
	}
	if entry.IsDir() {
		return nil
	}
	if !w.contained(path) {
		return nil
	}
	return w.add(path)
}

func (w *walker) contained(path string) bool {
	target, err := filepath.EvalSymlinks(path)
	if err != nil {
		return false
	}
	return !escapesRoot(w.root, target)
}

func (w *walker) excluded(path string, entry fs.DirEntry) bool {
	if entry.Type()&os.ModeSymlink != 0 || entry.IsDir() && entry.Name() == ".git" {
		return true
	}
	if entry.IsDir() {
		w.ignores.loadDir(w.cwd, path)
	}
	return w.ignores.ignored(w.cwd, path, entry.IsDir())
}

func skipEntry(entry fs.DirEntry) error {
	if entry.IsDir() {
		return filepath.SkipDir
	}
	return nil
}

func (w *walker) add(path string) error {
	rel, err := filepath.Rel(w.cwd, path)
	if err != nil {
		return fmt.Errorf("search: relative path %s: %w", path, err)
	}
	searchRel := filepath.Base(path)
	if path != w.scope {
		searchRel, err = filepath.Rel(w.scope, path)
		if err != nil {
			return fmt.Errorf("search: relative path %s: %w", path, err)
		}
	}
	w.entries = append(w.entries, fileEntry{
		absolute: path, display: filepath.ToSlash(rel), searchRel: filepath.ToSlash(searchRel),
	})
	return nil
}

func find(entries []fileEntry, args args) (outcome, error) {
	matched, err := matchingFiles(entries, args.Pattern)
	if err != nil {
		return outcome{}, err
	}
	paths := make([]string, 0, min(len(matched), args.Limit)+1)
	for _, entry := range matched[:min(len(matched), args.Limit)] {
		paths = append(paths, entry.display)
	}
	total := len(matched)
	if len(paths) == 0 {
		return outcome{Text: "No matches for " + args.Pattern}, nil
	}
	if total > len(paths) {
		paths = append(paths, fmt.Sprintf("[Truncated: %d more paths]", total-len(paths)))
	}
	return outcome{Text: strings.Join(paths, "\n")}, nil
}

func grep(entries []fileEntry, args args) (outcome, error) {
	matcher, err := grepRegexp(args)
	if err != nil {
		return outcome{}, err
	}
	files, err := matchingFiles(entries, cmp.Or(args.Glob, "**"))
	if err != nil {
		return outcome{}, err
	}
	scanned, kept, total := collectMatches(files, matcher, args.Limit)
	return renderGrep(scanned, kept, total, args.Pattern), nil
}

const grepWorkers = 8

type fileMatches struct {
	file  grepFile
	lines []int
}

func collectMatches(files []fileEntry, matcher *regexp.Regexp, limit int) ([]grepFile, []hit, int) {
	results := make([]fileMatches, len(files))
	workers := min(grepWorkers, len(files))
	var wg sync.WaitGroup
	for w := range workers {
		wg.Go(func() {
			for i := w; i < len(results); i += workers {
				file, lines := scanFile(files[i], matcher)
				results[i] = fileMatches{file: file, lines: lines}
			}
		})
	}
	wg.Wait()
	var scanned []grepFile
	var kept []hit
	total := 0
	for _, result := range results {
		if len(result.lines) == 0 {
			continue
		}
		fileIndex := len(scanned)
		scanned = append(scanned, result.file)
		for _, line := range result.lines {
			total++
			if len(kept) < limit {
				kept = append(kept, hit{file: fileIndex, line: line})
			}
		}
	}
	return scanned, kept, total
}

func grepRegexp(args args) (*regexp.Regexp, error) {
	pattern := args.Pattern
	if args.Literal {
		pattern = regexp.QuoteMeta(pattern)
	}
	if args.IgnoreCase {
		pattern = "(?i)" + pattern
	}
	re, err := regexp.Compile(pattern)
	if err != nil {
		return nil, fmt.Errorf("search: %w", err)
	}
	return re, nil
}

func matchingFiles(entries []fileEntry, pattern string) ([]fileEntry, error) {
	matcher, err := compileGlob(pattern, !strings.Contains(pattern, "/"))
	if err != nil {
		return nil, err
	}
	out := make([]fileEntry, 0, len(entries))
	for _, entry := range entries {
		if matcher.match(entry.searchRel) {
			out = append(out, entry)
		}
	}
	return out, nil
}

func scanFile(entry fileEntry, matcher *regexp.Regexp) (grepFile, []int) {
	if !searchableFile(entry.absolute) {
		return grepFile{}, nil
	}
	data, err := readCapped(entry.absolute)
	if err != nil || !searchableText(data) {
		return grepFile{}, nil
	}
	starts := splitStarts(data)
	var ordered []int
	for i := range starts {
		if matcher.Match(lineBytes(data, starts, i+1)) {
			ordered = append(ordered, i+1)
		}
	}
	return grepFile{entry: entry, data: data, starts: starts}, ordered
}

func searchableFile(path string) bool {
	info, err := os.Lstat(path)
	return err == nil && info.Mode().IsRegular() && info.Size() <= maxSearchFileBytes
}

func searchableText(data []byte) bool {
	return len(data) <= maxSearchFileBytes && bytes.IndexByte(data[:min(len(data), 8192)], 0) < 0 && utf8.Valid(data)
}

func readCapped(path string) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer func() { _ = file.Close() }()
	return io.ReadAll(io.LimitReader(file, maxSearchFileBytes+1))
}

func renderGrep(files []grepFile, kept []hit, total int, pattern string) outcome {
	if len(kept) == 0 {
		return outcome{Text: "No matches for " + pattern}
	}
	context := contextWidth(len(kept))
	rows := make([]string, 0, len(kept)*(context+1))
	captures := make([]capture, 0, len(files))
	for i, file := range files {
		marks := lineMarks(kept, i, len(file.starts), context)
		if len(marks) == 0 {
			continue
		}
		var shown []snapshot.Range
		rows, shown = appendFileRows(rows, file, marks)
		captures = append(captures, capture{Path: file.entry.display, Data: file.data, Shown: shown})
	}
	if total > len(kept) {
		rows = append(rows, fmt.Sprintf("[Truncated: %d more matches]", total-len(kept)))
	}
	return outcome{Text: strings.Join(rows, "\n"), Captures: captures}
}

func contextWidth(hits int) int {
	switch {
	case hits == 1:
		return 50
	case hits <= 3:
		return 15
	default:
		return 0
	}
}

func lineMarks(kept []hit, file, count, context int) map[int]byte {
	out := make(map[int]byte)
	for _, item := range kept {
		if item.file != file {
			continue
		}
		for line := max(1, item.line-context); line <= min(count, item.line+context); line++ {
			if out[line] != ':' {
				out[line] = '-'
			}
		}
		out[item.line] = ':'
	}
	return out
}

func appendFileRows(rows []string, file grepFile, marks map[int]byte) ([]string, []snapshot.Range) {
	shown := make([]snapshot.Range, 0, len(marks))
	for line := 1; line <= len(file.starts); line++ {
		marker, ok := marks[line]
		if !ok {
			continue
		}
		text, complete := cropMatch(string(lineBytes(file.data, file.starts, line)))
		rows = append(rows, fmt.Sprintf("%s:%d%c %s", file.entry.display, line, marker, text))
		if complete {
			shown = appendShown(shown, line)
		}
	}
	return rows, shown
}

func cropMatch(line string) (string, bool) {
	if utf8.RuneCountInString(line) <= 500 {
		return line, true
	}
	at := 0
	for range 500 {
		_, size := utf8.DecodeRuneInString(line[at:])
		at += size
	}
	return line[:at] + "...", false
}

func appendShown(ranges []snapshot.Range, line int) []snapshot.Range {
	if len(ranges) > 0 && ranges[len(ranges)-1].End+1 == line {
		ranges[len(ranges)-1].End = line
		return ranges
	}
	return append(ranges, snapshot.Range{Start: line, End: line})
}

func splitStarts(data []byte) []int {
	starts := make([]int, 0, bytes.Count(data, []byte("\n"))+1)
	start := 0
	for i, b := range data {
		if b != '\n' {
			continue
		}
		starts = append(starts, start)
		start = i + 1
	}
	if start < len(data) {
		starts = append(starts, start)
	}
	return starts
}

func lineBytes(data []byte, starts []int, line int) []byte {
	end := len(data)
	if line < len(starts) {
		end = starts[line] - 1
	} else if end > starts[line-1] && data[end-1] == '\n' {
		end--
	}
	chunk := data[starts[line-1]:end]
	if len(chunk) > 0 && chunk[len(chunk)-1] == '\r' {
		chunk = chunk[:len(chunk)-1]
	}
	return chunk
}
