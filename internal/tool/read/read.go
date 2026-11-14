package read

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const (
	defaultLimit = 2000
	maxFileBytes = 16 << 20
	maxLineBytes = 2000
	maxOutBytes  = 50 << 10
)

type Tool struct{}

type args struct {
	Path   string
	Offset int
	Limit  int
}

func New() tool.Tool { return Tool{} }

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "read",
		Description: "Read a file by line range. Copy source text without the <n>: prefixes. The final [path#tag] and [path@reference] lines identify the captured source for patch styles.",
		Schema:      jsontext.Value(`{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":2000}},"required":["path"],"additionalProperties":false}`),
		Keep:        tool.KeepHead,
	}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

var testHookReadSwap func()

func (Tool) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	parsed, err := parseArgs(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	path, display, err := resolveTarget(env, parsed.Path)
	if err != nil {
		return tool.Result{}, err
	}
	if testHookReadSwap != nil {
		testHookReadSwap()
	}
	if tool.IsLocalRunner(env.Runner) {
		return readPinnedFile(ctx, env, path, display, parsed)
	}
	info, err := env.Runner.Stat(ctx, path)
	if err != nil {
		return tool.Result{}, fmt.Errorf("read: %s does not exist: %w", parsed.Path, err)
	}
	if info.Mode.IsDir() {
		return readDirectory(ctx, env, path, display)
	}
	data, err := loadCheckedFile(ctx, env, path, display, info)
	if err != nil {
		return tool.Result{}, err
	}
	return render(data, display, parsed, env.Snapshots), nil
}

func loadCheckedFile(ctx context.Context, env tool.Env, path, display string, info runner.FileInfo) ([]byte, error) {
	if info.Size > maxFileBytes {
		return nil, fmt.Errorf("read: %s is a %d-byte text file; the limit is 16 MiB", display, info.Size)
	}
	data, err := env.Runner.ReadFile(ctx, path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", display, err)
	}
	if len(data) > maxFileBytes {
		return nil, fmt.Errorf("read: %s is a %d-byte text file; the limit is 16 MiB", display, len(data))
	}
	if binary(data) {
		return nil, fmt.Errorf("read: %s is a binary file (%d bytes)", display, len(data))
	}
	return data, nil
}

func readPinnedFile(ctx context.Context, env tool.Env, path, display string, parsed args) (tool.Result, error) {
	if err := ctx.Err(); err != nil {
		return tool.Result{}, fmt.Errorf("read: %w", err)
	}
	root := env.Cwd
	if filepath.IsAbs(parsed.Path) {
		root = env.Home
	}
	rel, err := filepath.Rel(root, path)
	if err != nil {
		return tool.Result{}, fmt.Errorf("read: cannot inspect %s: %w", display, err)
	}
	file, err := runner.OpenPinned(root, rel)
	if err != nil {
		return tool.Result{}, pinnedRefusal(display, parsed.Path, err)
	}
	defer func() { _ = file.Close() }()
	info, err := file.Stat()
	if err != nil {
		return tool.Result{}, fmt.Errorf("read %s: %w", display, err)
	}
	if info.IsDir() {
		return readPinnedDirectory(file, display)
	}
	data, err := readPinnedContents(file, info.Size(), display)
	if err != nil {
		return tool.Result{}, err
	}
	return render(data, display, parsed, env.Snapshots), nil
}

func readPinnedContents(file *os.File, size int64, display string) ([]byte, error) {
	if size > maxFileBytes {
		return nil, fmt.Errorf("read: %s is a %d-byte text file; the limit is 16 MiB", display, size)
	}
	data, err := io.ReadAll(io.LimitReader(file, maxFileBytes+1))
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", display, err)
	}
	if int64(len(data)) > maxFileBytes {
		return nil, fmt.Errorf("read: %s is a %d-byte text file; the limit is 16 MiB", display, len(data))
	}
	if binary(data) {
		return nil, fmt.Errorf("read: %s is a binary file (%d bytes)", display, len(data))
	}
	return data, nil
}

func pinnedRefusal(display, raw string, err error) error {
	if errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("read: %s does not exist: %w", raw, err)
	}
	if errors.Is(err, runner.ErrPinnedSymlink) {
		return fmt.Errorf("read: %s contains a symlink; refusing to follow it", display)
	}
	if errors.Is(err, runner.ErrPinnedEscape) {
		return fmt.Errorf("read: %s must stay within the workspace: %w", display, err)
	}
	return fmt.Errorf("read %s: %w", display, err)
}

func readDirectory(ctx context.Context, env tool.Env, path, display string) (tool.Result, error) {
	entries, err := env.Runner.ReadDir(ctx, path)
	if err != nil {
		return tool.Result{}, fmt.Errorf("read directory %s: %w", display, err)
	}
	return renderDirectory(entries, display), nil
}

func readPinnedDirectory(dir *os.File, display string) (tool.Result, error) {
	raw, err := dir.ReadDir(-1)
	if err != nil {
		return tool.Result{}, fmt.Errorf("read directory %s: %w", display, err)
	}
	entries := make([]runner.FileInfo, 0, len(raw))
	for _, entry := range raw {
		var mode fs.FileMode
		if entry.IsDir() {
			mode = fs.ModeDir
		}
		entries = append(entries, runner.FileInfo{Name: entry.Name(), Mode: mode})
	}
	return renderDirectory(entries, display), nil
}

func renderDirectory(entries []runner.FileInfo, display string) tool.Result {
	slices.SortFunc(entries, func(a, b runner.FileInfo) int {
		return cmp.Or(
			cmp.Compare(dirRank(b.Mode.IsDir()), dirRank(a.Mode.IsDir())),
			cmp.Compare(a.Name, b.Name),
		)
	})
	var out strings.Builder
	for i, entry := range entries {
		if i == 1000 {
			fmt.Fprintf(&out, "[Truncated: %d more entries]\n", len(entries)-i)
			break
		}
		out.WriteString(entry.Name)
		if entry.Mode.IsDir() {
			out.WriteByte('/')
		}
		out.WriteByte('\n')
	}
	return tool.Result{Output: []byte(out.String()), OK: true, Path: display}
}

func render(data []byte, path string, args args, store *snapshot.Store) tool.Result {
	var out strings.Builder
	out.Grow(min(len(data)+128, maxOutBytes))
	shown := writeWindow(&out, splitLines(data), args)
	fmt.Fprintf(&out, "[%s#%s]\n", path, snapshot.Tag("whole", data))
	fmt.Fprintf(&out, "[%s#%s]\n", path, snapshot.VersionTag(data))
	result := tool.Result{OK: true, Path: path}
	if entry, ok := store.Capture(path, data, shown); ok {
		fmt.Fprintf(&out, "[%s@%s]\n", snapshot.HeaderPath(path), entry.Ref)
		result.Meta = entry.Meta()
	}
	result.Output = []byte(out.String())
	return result
}

func writeWindow(out *strings.Builder, lines []string, args args) []snapshot.Range {
	shown := make([]snapshot.Range, 0, 1)
	end := min(len(lines), args.Offset-1+args.Limit)
	last := args.Offset - 1
	for ; last < end; last++ {
		text, complete := cropLine(lines[last])
		row := strconv.Itoa(last+1) + ":" + text + "\n"
		if out.Len()+len(row) > maxOutBytes-256 {
			break
		}
		out.WriteString(row)
		if complete {
			shown = appendShown(shown, last+1)
		}
	}
	switch {
	case args.Offset > 1 && args.Offset > len(lines):
		fmt.Fprintf(out, "[No lines at or after %d; file has %d lines.]\n", args.Offset, len(lines))
	case last < len(lines):
		fmt.Fprintf(out, "[Showing lines %d-%d of %d. Use %d to continue.]\n", args.Offset, last, len(lines), last+1)
	}
	return shown
}

func splitLines(data []byte) []string {
	if len(data) == 0 {
		return nil
	}
	raw := strings.Split(string(data), "\n")
	if len(raw) > 0 && raw[len(raw)-1] == "" {
		raw = raw[:len(raw)-1]
	}
	for i := range raw {
		raw[i] = strings.TrimSuffix(raw[i], "\r")
	}
	return raw
}

func cropLine(line string) (string, bool) {
	if len(line) <= maxLineBytes {
		return line, true
	}
	end := maxLineBytes
	for end > 0 && !utf8.RuneStart(line[end]) {
		end--
	}
	return line[:end] + "...", false
}

func appendShown(ranges []snapshot.Range, line int) []snapshot.Range {
	if len(ranges) > 0 && ranges[len(ranges)-1].End+1 == line {
		ranges[len(ranges)-1].End = line
		return ranges
	}
	return append(ranges, snapshot.Range{Start: line, End: line})
}

func binary(data []byte) bool {
	probe := data[:min(len(data), 8192)]
	return bytes.IndexByte(probe, 0) >= 0 || !utf8.Valid(data)
}

func ensureWithinWorkspace(cwd, path, display string) error {
	root, rootErr := filepath.EvalSymlinks(cwd)
	if rootErr != nil {
		return fmt.Errorf("read: cannot inspect %s: %w", display, rootErr)
	}
	target, targetErr := filepath.EvalSymlinks(path)
	if targetErr != nil {
		return fmt.Errorf("read: cannot inspect %s: %w", display, targetErr)
	}
	rel, relErr := filepath.Rel(root, target)
	if relErr != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return fmt.Errorf("read: %s escapes the workspace via symlink", display)
	}
	return nil
}

func resolvePath(cwd, home, raw string) (string, string, error) {
	if raw == "" {
		return "", "", errors.New("read: path must not be empty")
	}
	if strings.IndexByte(raw, 0) >= 0 {
		return "", "", errors.New("read: path contains a NUL byte")
	}
	if filepath.IsAbs(raw) {
		clean := filepath.Clean(raw)
		if !allowedSpill(home, clean) {
			return "", "", errors.New("read: absolute path is not a blob or process output spill")
		}
		return clean, filepath.ToSlash(clean), nil
	}
	clean := filepath.Clean(raw)
	if clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return "", "", errors.New("read: path must stay within the workspace")
	}
	return filepath.Join(cwd, clean), filepath.ToSlash(clean), nil
}

func resolveTarget(env tool.Env, raw string) (string, string, error) {
	path, display, err := resolvePath(env.Cwd, env.Home, raw)
	if err != nil {
		return "", "", err
	}
	if !filepath.IsAbs(raw) && tool.IsLocalRunner(env.Runner) {
		if err := ensureWithinWorkspace(env.Cwd, path, display); err != nil {
			return "", "", err
		}
	}
	return path, display, nil
}

var spillPattern = regexp.MustCompile(`^(?:blobs/[0-9a-f]{64}|runs/[^/]+/out|sessions/[^/]+/runs/[^/]+/out)$`)

func allowedSpill(home, path string) bool {
	if home == "" {
		return false
	}
	rel, err := filepath.Rel(home, path)
	return err == nil && spillPattern.MatchString(filepath.ToSlash(rel))
}

func parseArgs(raw jsontext.Value) (args, error) {
	decoder := jsontext.NewDecoder(bytes.NewBuffer(raw))
	start, err := decoder.ReadToken()
	if err != nil || start.Kind() != '{' {
		return args{}, errors.New("read: arguments must be an object")
	}
	out := args{Offset: 1, Limit: defaultLimit}
	for decoder.PeekKind() != '}' {
		if fieldErr := readArg(decoder, &out); fieldErr != nil {
			return args{}, fieldErr
		}
	}
	if _, endErr := decoder.ReadToken(); endErr != nil {
		return args{}, fmt.Errorf("read: invalid arguments: %w", endErr)
	}
	if _, tailErr := decoder.ReadToken(); !errors.Is(tailErr, io.EOF) {
		return args{}, errors.New("read: arguments have trailing data")
	}
	return out, validateArgs(out)
}

func readArg(decoder *jsontext.Decoder, out *args) error {
	name, err := decoder.ReadToken()
	if err != nil {
		return fmt.Errorf("read: invalid arguments: %w", err)
	}
	key := name.String()
	value, err := decoder.ReadValue()
	if err != nil {
		return fmt.Errorf("read: invalid arguments: %w", err)
	}
	switch key {
	case "path":
		out.Path, err = stringArg(value)
	case "offset":
		out.Offset, err = intArg(value)
	case "limit":
		out.Limit, err = intArg(value)
	default:
		return fmt.Errorf("read: unknown argument %q", key)
	}
	if err != nil {
		return fmt.Errorf("read: %s: %w", key, err)
	}
	return nil
}

func validateArgs(out args) error {
	if out.Path == "" {
		return errors.New("read: path must not be empty")
	}
	if out.Offset < 1 {
		return errors.New("read: offset must be at least 1")
	}
	if out.Limit < 1 || out.Limit > defaultLimit {
		return errors.New("read: limit must be between 1 and 2000")
	}
	return nil
}

func stringArg(value jsontext.Value) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != '"' {
		return "", errors.New("must be a string")
	}
	return token.String(), nil
}

func intArg(value jsontext.Value) (int, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, err
	}
	if token.Kind() != '0' {
		return 0, errors.New("must be an integer")
	}
	number, err := strconv.Atoi(token.String())
	if err != nil {
		return 0, fmt.Errorf("must be an integer: %w", err)
	}
	return number, nil
}

func dirRank(dir bool) int {
	if dir {
		return 1
	}
	return 0
}
