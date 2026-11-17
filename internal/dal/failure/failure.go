package failure

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

type Class string

const (
	ClassPanic   Class = "panic"
	ClassTool    Class = "tool"
	ClassModel   Class = "model"
	ClassJournal Class = "journal"
	ClassRuntime Class = "runtime"
)

type Origin string

const (
	OriginMain   Origin = "main"
	OriginSide   Origin = "side"
	OriginDoctor Origin = "doctor"
	OriginTest   Origin = "test"
)

type Frame struct {
	Function string `json:"function"`
	File     string `json:"file"`
	Line     int    `json:"line"`
}

type Record struct {
	SchemaVersion int          `json:"schemaVersion"`
	ID            string       `json:"id"`
	Time          time.Time    `json:"time"`
	Product       string       `json:"product"`
	Version       string       `json:"version"`
	OS            string       `json:"os"`
	Arch          string       `json:"arch"`
	Class         Class        `json:"class"`
	Tool          string       `json:"tool"`
	Error         string       `json:"error"`
	Stack         []Frame      `json:"stack"`
	Args          string       `json:"args"`
	Output        string       `json:"output"`
	Fingerprint   string       `json:"fingerprint"`
	ContentFree   bool         `json:"contentFree"`
	Reportable    bool         `json:"reportable"`
	FailingStep   *FailureStep `json:"failingStep"`
}

type Input struct {
	ID          string
	Time        time.Time
	Product     string
	Version     string
	Class       Class
	Tool        string
	Err         error
	Frames      []Frame
	Args        string
	Output      string
	Workspace   string
	Origin      Origin
	Correlation Correlation
	FailingStep *FailureStep
}

type Masker interface {
	MaskWithError(string) (string, error)
}

const schemaVersion = 1

const (
	maxErrorBytes   = 4096
	maxArgsBytes    = 2048
	maxOutputBytes  = 4096
	maxStepArgs     = 8192
	maxStepJSON     = 16384
	maxRecordBytes  = 65536
	maxSidecarBytes = 4096
	maxRawField     = 1 << 20
	maxFrames       = 32
	keptFrames      = 16
	maxFuncBytes    = 256
	maxFileBytes    = 256
	maxRecords      = 256
	maxCommitted    = 8 << 20
	maxAge          = 14 * 24 * time.Hour
	stagingMaxAge   = 24 * time.Hour
)

const truncatedMarker = "[truncated]"

var (
	toolPattern    = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,64}$`)
	versionPattern = regexp.MustCompile(`^[A-Za-z0-9_.+-]{1,80}$`)
	recordName     = regexp.MustCompile(`^failure-[0-9a-f]{32}\.json$`)
	idPattern      = regexp.MustCompile(`^[0-9a-f]{32}$`)
)

func NewID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("failure: random id: %w", err)
	}
	return hex.EncodeToString(raw[:]), nil
}

func validProduct(product string) bool {
	return product == "dal" || product == "dalgona"
}

func validClass(class Class) bool {
	switch class {
	case ClassPanic, ClassTool, ClassModel, ClassJournal, ClassRuntime:
		return true
	}
	return false
}

func validOrigin(origin Origin) bool {
	switch origin {
	case OriginMain, OriginSide, OriginDoctor, OriginTest:
		return true
	}
	return false
}

func normalizeTool(name string) string {
	if name == "" {
		return ""
	}
	if toolPattern.MatchString(name) {
		return name
	}
	return "unknown"
}

func normalizeVersion(version string) string {
	if version == "" {
		return "unknown"
	}
	if versionPattern.MatchString(version) {
		return version
	}
	return "unknown"
}

func Build(ctx context.Context, input Input, masker Masker) (Record, error) {
	if err := ctx.Err(); err != nil {
		return Record{}, err
	}
	if !validProduct(input.Product) || !validClass(input.Class) || !validOrigin(input.Origin) {
		return Record{}, errors.New("failure: invalid record")
	}
	id, err := resolveRecordID(input.ID)
	if err != nil {
		return Record{}, err
	}
	at := input.Time.UTC()
	if at.IsZero() {
		at = time.Now().UTC()
	}
	version := normalizeVersion(input.Version)
	name := normalizeTool(input.Tool)
	masked, ok := maskRecordFields(input, masker)
	if !ok {
		return contentFree(contentFreeParams{product: input.Product, version: version, name: name, class: input.Class, id: id, at: at, origin: input.Origin}), nil
	}
	record := Record{
		SchemaVersion: schemaVersion,
		ID:            id,
		Time:          at,
		Product:       input.Product,
		Version:       version,
		OS:            runtime.GOOS,
		Arch:          runtime.GOARCH,
		Class:         input.Class,
		Tool:          name,
		Error:         truncateUTF8(redactSecrets(scrubPrivacy(masked.errText, input.Workspace, input.Product)), maxErrorBytes),
		Stack:         normalizeFrames(input.Frames),
		Args:          truncateUTF8(redactSecrets(scrubPrivacy(masked.args, input.Workspace, input.Product)), maxArgsBytes),
		Output:        truncateUTF8(redactSecrets(scrubPrivacy(masked.output, input.Workspace, input.Product)), maxOutputBytes),
		FailingStep:   input.FailingStep,
	}
	record.Fingerprint = Fingerprint(record)
	record.Reportable = input.Product == "dalgona" && input.Origin == OriginMain
	return record, nil
}

func resolveRecordID(inputID string) (string, error) {
	if inputID == "" {
		generated, err := NewID()
		if err != nil {
			return "", err
		}
		return generated, nil
	}
	if !idPattern.MatchString(inputID) {
		return "", errors.New("failure: invalid record")
	}
	return inputID, nil
}

func maskRecordFields(input Input, masker Masker) (masked maskedRecordFields, ok bool) {
	message := ""
	if input.Err != nil {
		message = input.Err.Error()
	}
	if len(message) > maxRawField || len(input.Args) > maxRawField || len(input.Output) > maxRawField {
		return maskedRecordFields{}, false
	}
	if masker == nil {
		return maskedRecordFields{}, false
	}
	errText, err := masker.MaskWithError(message)
	if err != nil {
		return maskedRecordFields{}, false
	}
	argsText, err := maskArgs(masker, input.Tool, input.Args, input.Workspace)
	if err != nil {
		return maskedRecordFields{}, false
	}
	masked = maskedRecordFields{errText: errText, args: argsText}
	if !maskRecordOutput(input, masker, &masked) {
		return maskedRecordFields{}, false
	}
	return masked, true
}

type maskedRecordFields struct {
	errText string
	args    string
	output  string
}

func maskRecordOutput(input Input, masker Masker, masked *maskedRecordFields) bool {
	if !keepOutput(input.Tool) || input.Output == "" {
		return true
	}
	outText, err := masker.MaskWithError(input.Output)
	if err != nil {
		return false
	}
	masked.output = outText
	return true
}

type contentFreeParams struct {
	product string
	version string
	name    string
	class   Class
	id      string
	at      time.Time
	origin  Origin
}

func contentFree(params contentFreeParams) Record {
	record := Record{
		SchemaVersion: schemaVersion,
		ID:            params.id,
		Time:          params.at,
		Product:       params.product,
		Version:       params.version,
		OS:            runtime.GOOS,
		Arch:          runtime.GOARCH,
		Class:         params.class,
		Tool:          params.name,
		Error:         "failure details withheld: masking failed",
		ContentFree:   true,
		FailingStep: &FailureStep{
			SchemaVersion: schemaVersion,
			Status:        StatusUnavailable,
			Reason:        ReasonMaskingFailed,
		},
	}
	record.Fingerprint = Fingerprint(record)
	record.Reportable = params.product == "dalgona" && params.origin == OriginMain
	return record
}

func Fingerprint(record Record) string {
	fields := []string{"failure-v1", record.Product, string(record.Class), record.Tool}
	frames := normalizeFrames(record.Stack)
	if len(frames) == 0 {
		fields = append(fields, "no-stack")
	}
	for _, frame := range frames {
		fields = append(fields, frame.Function+"@"+frame.File)
	}
	sum := sha256.Sum256([]byte(strings.Join(fields, "\x00")))
	return hex.EncodeToString(sum[:])
}

func truncateUTF8(s string, limit int) string {
	if len(s) <= limit {
		return s
	}
	cut := limit - len(truncatedMarker)
	if cut < 0 {
		cut = 0
	}
	for cut > 0 && !utf8.ValidString(s[:cut]) {
		cut--
	}
	return s[:cut] + truncatedMarker
}

type secretSpan struct {
	start int
	end   int
	match string
}

func redactSecrets(text string) string {
	if text == "" {
		return ""
	}
	spans := secretSpans(text)
	kept := mergeSecretSpans(spans, len(text))
	return applySecretSpans(text, kept)
}

func secretSpans(text string) []secretSpan {
	hits := secrets.Scan(text)
	spans := make([]secretSpan, 0, len(hits))
	for _, hit := range hits {
		if hit.Start < 0 || hit.End > len(text) || hit.Start >= hit.End {
			continue
		}
		spans = append(spans, secretSpan{start: hit.Start, end: hit.End, match: text[hit.Start:hit.End]})
	}
	slices.SortFunc(spans, func(a, b secretSpan) int {
		if a.end-a.start != b.end-b.start {
			return (b.end - b.start) - (a.end - a.start)
		}
		if a.match != b.match {
			return strings.Compare(a.match, b.match)
		}
		return a.start - b.start
	})
	return spans
}

func mergeSecretSpans(spans []secretSpan, length int) []secretSpan {
	var kept []secretSpan
	covered := make([]bool, length)
	for _, candidate := range spans {
		if spanOverlaps(covered, candidate) {
			continue
		}
		for i := candidate.start; i < candidate.end; i++ {
			covered[i] = true
		}
		kept = append(kept, candidate)
	}
	slices.SortFunc(kept, func(a, b secretSpan) int { return a.start - b.start })
	return kept
}

func spanOverlaps(covered []bool, candidate secretSpan) bool {
	for i := candidate.start; i < candidate.end; i++ {
		if covered[i] {
			return true
		}
	}
	return false
}

func applySecretSpans(text string, kept []secretSpan) string {
	var out strings.Builder
	out.Grow(len(text))
	pos := 0
	for _, done := range kept {
		out.WriteString(text[pos:done.start])
		out.WriteString("[redacted]")
		pos = done.end
	}
	out.WriteString(text[pos:])
	return out.String()
}

var (
	tokenPathPattern  = regexp.MustCompile(`(^|[\s"'` + "`" + `(=;|])((?:/[^\s"'` + "`" + `\\|&;()<>]*)|(?:~(?:/[^\s"'` + "`" + `]*)?))`)
	drivePathPattern  = regexp.MustCompile(`[A-Za-z]:[\\/][^ \t\n"'` + "`" + `]*`)
	uncPathPattern    = regexp.MustCompile(`\\\\[^\s"'` + "`" + `]+`)
	quotedPathPattern = regexp.MustCompile(`"((?:[^"\\]|\\.)*)"`)
	urlPattern        = regexp.MustCompile(`[A-Za-z][A-Za-z0-9+.-]*:(?://|\\/\\/)(?:[^\s"'<>\\]|\\.)*`)
	envPattern        = regexp.MustCompile(`\b[A-Za-z_][A-Za-z0-9_]{0,63}=[^\s"'` + "`" + `;&|]+`)
	emailPattern      = regexp.MustCompile(`\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+\b`)
)

func scrubPrivacy(text, workspace, product string) string {
	if text == "" {
		return ""
	}
	out := scrubWorkspacePaths(text, workspace)
	masked, urls := maskPrivacyURLs(out)
	masked = scrubEnvAssignments(masked)
	masked = emailPattern.ReplaceAllString(masked, "[redacted]")
	masked = scrubAbsolutePaths(masked)
	masked = scrubWorkspaceBase(masked, workspace, product)
	return restorePrivacyURLs(masked, urls)
}

func scrubWorkspacePaths(text, workspace string) string {
	if workspace == "" {
		return text
	}
	slashWorkspace := filepath.ToSlash(workspace)
	out := quotedPathPattern.ReplaceAllStringFunc(text, func(match string) string {
		return scrubQuotedMatch(match, slashWorkspace)
	})
	return replaceWorkspaceBare(out, slashWorkspace)
}

func scrubQuotedMatch(match, slashWorkspace string) string {
	inner := match[1 : len(match)-1]
	plain := strings.ReplaceAll(strings.ReplaceAll(inner, `\/`, "/"), `\\`, `\`)
	if rel, ok := workspaceRelative(plain, slashWorkspace); ok {
		return `"` + strings.ReplaceAll(rel, `\`, `\\`) + `"`
	}
	if isAbsolutePath(plain) {
		return `"[path]"`
	}
	return match
}

func maskPrivacyURLs(text string) (string, []string) {
	var urls []string
	masked := urlPattern.ReplaceAllStringFunc(text, func(match string) string {
		urls = append(urls, cleanURL(match))
		return "\x00" + strconv.Itoa(len(urls)-1) + "\x00"
	})
	return masked, urls
}

func scrubEnvAssignments(text string) string {
	return envPattern.ReplaceAllStringFunc(text, scrubEnvMatch)
}

func scrubEnvMatch(match string) string {
	name, value, _ := strings.Cut(match, "=")
	value = strings.TrimRight(value, ".,;:!?)")
	if value == "" {
		return match
	}
	return name + "=[redacted]"
}

func scrubAbsolutePaths(text string) string {
	out := tokenPathPattern.ReplaceAllString(text, "${1}[path]")
	out = drivePathPattern.ReplaceAllString(out, "[path]")
	return uncPathPattern.ReplaceAllString(out, "[path]")
}

func scrubWorkspaceBase(text, workspace, product string) string {
	if workspace == "" {
		return text
	}
	base := filepath.Base(filepath.ToSlash(workspace))
	if len(base) >= 3 && base != product && base != "." && base != "/" {
		return strings.ReplaceAll(text, base, "[path]")
	}
	return text
}

func restorePrivacyURLs(text string, urls []string) string {
	for i, cleaned := range urls {
		text = strings.ReplaceAll(text, "\x00"+strconv.Itoa(i)+"\x00", cleaned)
	}
	return text
}

func replaceWorkspaceBare(text, slashWorkspace string) string {
	var out strings.Builder
	rest := text
	for {
		index := strings.Index(rest, slashWorkspace)
		if index < 0 {
			out.WriteString(rest)
			return out.String()
		}
		rest = advanceBareSegment(&out, rest, index, slashWorkspace)
	}
}

func advanceBareSegment(out *strings.Builder, rest string, index int, slashWorkspace string) string {
	after := rest[index+len(slashWorkspace):]
	if after != "" && after[0] != '/' {
		out.WriteString(rest[:index+len(slashWorkspace)])
		return after
	}
	rel := bareRelText(after)
	if rel == "" {
		out.WriteString(rest[:index])
		out.WriteString("[path]")
		return after
	}
	if rel == ".." || strings.HasPrefix(rel, "../") {
		out.WriteString(rest[:index])
		out.WriteString("[path]")
		return rest[index+len(slashWorkspace)+1+len(rel):]
	}
	out.WriteString(rest[:index])
	out.WriteString(rel)
	return rest[index+len(slashWorkspace)+1+len(rel):]
}

func bareRelText(after string) string {
	rel := strings.TrimPrefix(after, "/")
	end := len(rel)
	for i, c := range rel {
		if c == ' ' || c == '\t' || c == '\n' || c == '"' || c == '\'' || c == '`' {
			end = i
			break
		}
	}
	return rel[:end]
}

func workspaceRelative(path, slashWorkspace string) (string, bool) {
	plain := filepath.ToSlash(path)
	if plain == slashWorkspace {
		return ".", true
	}
	rel, err := filepath.Rel(slashWorkspace, plain)
	if err != nil {
		return "", false
	}
	rel = filepath.ToSlash(rel)
	if rel == ".." || strings.HasPrefix(rel, "../") {
		return "", false
	}
	return rel, true
}

func isAbsolutePath(path string) bool {
	if strings.HasPrefix(path, "/") || strings.HasPrefix(path, `\\`) || strings.HasPrefix(path, "~") {
		return true
	}
	if len(path) >= 3 && path[1] == ':' && (path[2] == '/' || path[2] == '\\') {
		return true
	}
	return false
}

func cleanURL(match string) string {
	schemeEnd := strings.Index(match, ":")
	if schemeEnd < 0 {
		return match
	}
	scheme := match[:schemeEnd]
	rest := match[schemeEnd+1:]
	rest = strings.ReplaceAll(rest, `\/`, "/")
	rest = strings.TrimPrefix(rest, "//")
	host := rest
	tail := ""
	for i, c := range rest {
		if c == '/' || c == '?' || c == '#' {
			host = rest[:i]
			tail = rest[i:]
			break
		}
	}
	if at := strings.LastIndex(host, "@"); at >= 0 {
		host = host[at+1:]
	}
	path := tail
	if cut := strings.IndexAny(path, "?#"); cut >= 0 {
		path = path[:cut]
	}
	if host == "" {
		return match
	}
	return scheme + "://" + host + path
}

func maskArgs(masker Masker, name, args, workspace string) (string, error) {
	if args == "" {
		return "", nil
	}
	var decoded any
	if err := json.Unmarshal([]byte(args), &decoded); err != nil {
		return maskRawArgs(masker, name, args)
	}
	projected, refusal := projectArgs(name, decoded, workspace)
	if refusal == ReasonPrivateContent || refusal == ReasonPreimage {
		return "", nil
	}
	if projected == nil {
		return masker.MaskWithError(args)
	}
	masked, err := maskDecoded(masker, projected)
	if err != nil {
		return "", err
	}
	encoded, encodeErr := json.Marshal(masked)
	if encodeErr != nil {
		return "", encodeErr
	}
	return masker.MaskWithError(string(encoded))
}

func maskRawArgs(masker Masker, name, args string) (string, error) {
	if name == "patch" || name == "rewrite" {
		return "", nil
	}
	return masker.MaskWithError(args)
}

func maskDecoded(masker Masker, value any) (any, error) {
	switch typed := value.(type) {
	case map[string]any:
		return maskDecodedMap(masker, typed)
	case []any:
		return maskDecodedSlice(masker, typed)
	case string:
		return masker.MaskWithError(typed)
	default:
		return value, nil
	}
}

func maskDecodedMap(masker Masker, typed map[string]any) (any, error) {
	out := make(map[string]any, len(typed))
	for key, item := range typed {
		maskedKey, keyErr := masker.MaskWithError(key)
		if keyErr != nil {
			return nil, keyErr
		}
		maskedItem, itemErr := maskDecoded(masker, item)
		if itemErr != nil {
			return nil, itemErr
		}
		out[maskedKey] = maskedItem
	}
	return out, nil
}

func maskDecodedSlice(masker Masker, typed []any) (any, error) {
	out := make([]any, 0, len(typed))
	for _, item := range typed {
		masked, err := maskDecoded(masker, item)
		if err != nil {
			return nil, err
		}
		out = append(out, masked)
	}
	return out, nil
}

func keepOutput(name string) bool {
	switch name {
	case "patch", "rewrite", "todo":
		return true
	}
	return false
}

func normalizeFrames(frames []Frame) []Frame {
	out := make([]Frame, 0, len(frames))
	for _, frame := range frames {
		function := frame.Function
		if index := strings.Index(function, "["); index >= 0 && strings.HasSuffix(function, "]") {
			function = function[:index]
		}
		if strings.HasPrefix(function, "runtime.") {
			continue
		}
		if strings.Contains(function, "internal/dal/failure.") {
			continue
		}
		out = append(out, Frame{
			Function: truncateUTF8(function, maxFuncBytes),
			File:     truncateUTF8(mapFailureFile(frame.File), maxFileBytes),
			Line:     frame.Line,
		})
		if len(out) >= keptFrames {
			break
		}
	}
	return out
}

func mapFailureFile(path string) string {
	if mapped, ok := mapFailurePkgPath(path); ok {
		return mapped
	}
	if mapped, ok := mapFailureMarkerPath(path); ok {
		return mapped
	}
	return mapFailureStdPath(path)
}

func mapFailurePkgPath(path string) (string, bool) {
	if strings.HasPrefix(path, "std/") || strings.HasPrefix(path, "[external]/") {
		return path, true
	}
	if index := strings.Index(path, "github.com/gosuda/dalgona/"); index >= 0 {
		return path[index+len("github.com/gosuda/dalgona/"):], true
	}
	return "", false
}

func mapFailureMarkerPath(path string) (string, bool) {
	if !filepath.IsAbs(path) && !isAbsolutePath(path) {
		return path, true
	}
	for _, marker := range []string{"/internal/dal/", "/internal/dalgona/", "/internal/llmstub/", "/internal/commentgate/", "/internal/pingate/", "/cmd/"} {
		if index := strings.Index(path, marker); index >= 0 {
			return strings.TrimPrefix(path[index+1:], "/"), true
		}
	}
	return "", false
}

func mapFailureStdPath(path string) string {
	if goroot := runtime.GOROOT(); goroot != "" { //nolint:staticcheck // build-time GOROOT matches embedded stack paths
		if rel, err := filepath.Rel(filepath.Join(goroot, "src"), path); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return "std/" + filepath.ToSlash(rel)
		}
	}
	return "[external]/" + filepath.ToSlash(filepath.Base(path))
}

func CaptureFrames(skip int) []Frame {
	pcs := make([]uintptr, 64)
	count := runtime.Callers(skip+2, pcs)
	callers := runtime.CallersFrames(pcs[:count])
	var frames []Frame
	for {
		caller, more := callers.Next()
		frames = append(frames, Frame{Function: caller.Function, File: caller.File, Line: caller.Line})
		if !more {
			break
		}
	}
	return frames
}

type Options struct {
	Home    string
	Product string
	Version string
	Masker  Masker
	Now     func() time.Time
	Notice  io.Writer
}

type Query struct {
	ID    string
	Limit int
}

type Summary struct {
	Count  int
	Bytes  int64
	Latest time.Time
}

type frozenRecord struct {
	record  Record
	encoded []byte
	local   []byte
}

type Store struct {
	dir       string
	product   string
	version   string
	masker    Masker
	now       func() time.Time
	notice    io.Writer
	recordsMu sync.Mutex
	frozen    map[string]frozenRecord
	notified  map[string]bool
}

func Open(ctx context.Context, options Options) (*Store, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if options.Home == "" {
		return nil, errors.New("failure: home is empty")
	}
	if info, err := os.Lstat(options.Home); err == nil {
		if info.Mode()&fs.ModeSymlink != 0 {
			return nil, errors.New("failure: directory is not private")
		}
	} else if !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("failure: directory is not private: %w", err)
	}
	dir := filepath.Join(options.Home, "failures")
	if err := ensurePrivateDir(dir); err != nil {
		return nil, err
	}
	now := options.Now
	if now == nil {
		now = time.Now
	}
	store := &Store{
		dir:      dir,
		product:  options.Product,
		version:  options.Version,
		masker:   options.Masker,
		now:      now,
		notice:   options.Notice,
		frozen:   map[string]frozenRecord{},
		notified: map[string]bool{},
	}
	store.removeStaging()
	return store, nil
}

func ownedFile(dir, name string) bool {
	info, err := os.Lstat(filepath.Join(dir, name))
	if err != nil {
		return false
	}
	if info.Mode()&fs.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return false
	}
	return true
}

var errOccurrenceConflict = errors.New("failure: occurrence id conflict")

func isStorageError(err error) bool {
	return err != nil && strings.HasPrefix(err.Error(), "failure: write local record")
}

func ensurePrivateDir(dir string) error {
	info, err := os.Lstat(dir)
	if err != nil {
		if !errors.Is(err, fs.ErrNotExist) {
			return fmt.Errorf("failure: directory is not private: %w", err)
		}
		info, err = createPrivateDir(dir)
		if err != nil {
			return err
		}
	}
	return checkPrivateDirInfo(info)
}

func createPrivateDir(dir string) (os.FileInfo, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("failure: directory is not private: %w", err)
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return nil, fmt.Errorf("failure: directory is not private: %w", err)
	}
	return info, nil
}

func checkPrivateDirInfo(info os.FileInfo) error {
	if info.Mode()&fs.ModeSymlink != 0 || !info.IsDir() {
		return errors.New("failure: directory is not private")
	}
	if runtime.GOOS != "windows" {
		return checkPrivateDirPerm(info)
	}
	return nil
}

func checkPrivateDirPerm(info os.FileInfo) error {
	if info.Mode().Perm()&0o077 != 0 {
		return errors.New("failure: directory is not private")
	}
	if stat, ok := info.Sys().(interface{ Uid() uint32 }); ok {
		if int(stat.Uid()) != os.Getuid() {
			return errors.New("failure: directory is not private")
		}
	}
	return nil
}

func recordFilename(id string) string {
	return "failure-" + id + ".json"
}

func localFilename(id string) string {
	return "failure-" + id + ".local.json"
}

func (s *Store) Capture(ctx context.Context, input Input) (Record, error) {
	if err := ctx.Err(); err != nil {
		return Record{}, err
	}
	s.recordsMu.Lock()
	defer s.recordsMu.Unlock()
	return s.captureLocked(ctx, input, s.masker)
}

func (s *Store) Recover(ctx context.Context, input Input, errp *error) {
	value := recover()
	if value == nil {
		return
	}
	formatted := ""
	func() {
		defer func() { _ = recover() }()
		formatted = fmt.Sprint(value)
	}()
	input.Class = ClassPanic
	input.Frames = CaptureFrames(2)
	masker := s.masker
	if formatted == "" {
		input.Err = errors.New("panic value could not be formatted")
		masker = nil
	} else {
		input.Err = errors.New(formatted)
	}
	s.recordsMu.Lock()
	defer s.recordsMu.Unlock()
	_, _ = s.captureLocked(ctx, input, masker)
	if errp != nil {
		*errp = errors.New("failure: recovered panic")
	}
}

func (s *Store) captureLocked(ctx context.Context, input Input, masker Masker) (Record, error) {
	id := input.ID
	if id == "" {
		generated, err := NewID()
		if err != nil {
			return Record{}, err
		}
		id = generated
	}
	if kept, ok := s.frozen[id]; ok {
		return s.replayFrozen(id, kept), nil
	}
	input.ID = id
	built, err := s.build(ctx, input, masker)
	if err != nil {
		if isStorageError(err) {
			s.notify("failure log unavailable; details were not written")
		}
		return Record{}, err
	}
	if handled, record, reuseErr := s.reuseCommitted(id, built); reuseErr != nil || handled {
		return record, reuseErr
	}
	if rotateErr := s.rotate(built.encoded, built.local); rotateErr != nil {
		s.notify("failure log unavailable; details were not written")
		return Record{}, fmt.Errorf("failure: write local record: %w", rotateErr)
	}
	return s.commitPublished(id, built)
}

func (s *Store) replayFrozen(id string, kept frozenRecord) Record {
	s.writeFileIfMissing(recordFilename(id), kept.encoded)
	s.writeFileIfMissing(localFilename(id), kept.local)
	return kept.record
}

func (s *Store) reuseCommitted(id string, built builtStoreRecord) (bool, Record, error) {
	existing, readErr := os.ReadFile(filepath.Join(s.dir, recordFilename(id)))
	if readErr != nil {
		return false, Record{}, nil
	}
	if string(existing) == string(built.encoded) {
		s.frozen[id] = frozenRecord(built)
		s.writeFileIfMissing(localFilename(id), built.local)
		return true, built.record, nil
	}
	return true, Record{}, errOccurrenceConflict
}

func (s *Store) commitPublished(id string, built builtStoreRecord) (Record, error) {
	if err := s.publish(built); err != nil {
		if errors.Is(err, errOccurrenceConflict) {
			return Record{}, err
		}
		s.notify("failure log unavailable; details were not written")
		return Record{}, fmt.Errorf("failure: write local record: %w", err)
	}
	s.frozen[id] = frozenRecord(built)
	s.notify("failure recorded; run " + s.product + " failures")
	s.notified[id] = true
	return built.record, nil
}

func (s *Store) notify(message string) {
	if s.notice == nil {
		return
	}
	_, _ = io.WriteString(s.notice, message+"\n")
}

type builtStoreRecord struct {
	record  Record
	encoded []byte
	local   []byte
}

func (s *Store) build(ctx context.Context, input Input, masker Masker) (builtStoreRecord, error) {
	if input.Time.IsZero() {
		input.Time = s.now().UTC()
	}
	record, err := Build(ctx, input, masker)
	if err != nil {
		return builtStoreRecord{}, err
	}
	encoded, err := json.Marshal(record)
	if err != nil {
		return builtStoreRecord{}, fmt.Errorf("failure: write local record: %w", err)
	}
	if len(encoded) > maxRecordBytes {
		fallback, fallbackEncoded, fallbackErr := s.contentFreeEncoded(record)
		if fallbackErr != nil {
			return builtStoreRecord{}, fallbackErr
		}
		record, encoded = fallback, fallbackEncoded
	}
	local, err := s.localBytes(record, input)
	if err != nil {
		return builtStoreRecord{}, err
	}
	return builtStoreRecord{record: record, encoded: encoded, local: local}, nil
}

func (s *Store) contentFreeEncoded(record Record) (Record, []byte, error) {
	fallback := contentFree(contentFreeParams{product: record.Product, version: record.Version, name: record.Tool, class: record.Class, id: record.ID, at: record.Time, origin: OriginMain})
	fallback.Reportable = record.Reportable
	encoded, err := json.Marshal(fallback)
	if err != nil {
		return Record{}, nil, fmt.Errorf("failure: write local record: %w", err)
	}
	return fallback, encoded, nil
}

func (s *Store) localBytes(record Record, input Input) ([]byte, error) {
	argsSum := sha256.Sum256([]byte(input.Args))
	outSum := sha256.Sum256([]byte(input.Output))
	local := LocalRecord{
		SchemaVersion:      schemaVersion,
		FailureID:          record.ID,
		Correlation:        input.Correlation,
		RepairedArgsSHA256: hex.EncodeToString(argsSum[:]),
		OutputBytes:        int64(len(input.Output)),
		OutputSHA256:       hex.EncodeToString(outSum[:]),
		OS:                 runtime.GOOS,
		Arch:               runtime.GOARCH,
	}
	encoded, err := json.Marshal(local)
	if err != nil {
		return nil, fmt.Errorf("failure: write local record: %w", err)
	}
	if len(encoded) > maxSidecarBytes {
		return nil, fmt.Errorf("failure: write local record: sidecar exceeds its limit")
	}
	return encoded, nil
}

func (s *Store) publish(built builtStoreRecord) error {
	root, err := os.OpenRoot(s.dir)
	if err != nil {
		return err
	}
	defer func() { _ = root.Close() }()
	random, err := publishRandom()
	if err != nil {
		return err
	}
	staging := ".pending-" + built.record.ID + "-" + hex.EncodeToString(random[:])
	if err := writeStagedRecord(root, staging, built.encoded); err != nil {
		return err
	}
	return s.linkStagedRecord(root, staging, built)
}

func publishRandom() ([8]byte, error) {
	var random [8]byte
	if _, err := rand.Read(random[:]); err != nil {
		return [8]byte{}, err
	}
	return random, nil
}

func writeStagedRecord(root *os.Root, staging string, encoded []byte) error {
	staged, err := root.OpenFile(staging, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	_, writeErr := staged.Write(encoded)
	syncErr := staged.Sync()
	closeErr := staged.Close()
	if writeErr != nil || syncErr != nil {
		_ = root.Remove(staging)
		return errors.Join(writeErr, syncErr, closeErr)
	}
	if closeErr != nil {
		_ = root.Remove(staging)
		return closeErr
	}
	return nil
}

func (s *Store) linkStagedRecord(root *os.Root, staging string, built builtStoreRecord) error {
	target := filepath.Join(s.dir, recordFilename(built.record.ID))
	if err := os.Link(filepath.Join(s.dir, staging), target); err != nil {
		_ = root.Remove(staging)
		return s.linkConflict(target, root, built, err)
	}
	_ = root.Remove(staging)
	s.syncParent()
	return s.publishSidecar(root, built.record.ID, built.local)
}

func (s *Store) linkConflict(target string, root *os.Root, built builtStoreRecord, linkErr error) error {
	if !errors.Is(linkErr, fs.ErrExist) {
		return linkErr
	}
	existing, readErr := os.ReadFile(target)
	if readErr != nil {
		return readErr
	}
	if string(existing) != string(built.encoded) {
		return errOccurrenceConflict
	}
	return s.publishSidecar(root, built.record.ID, built.local)
}

func (s *Store) publishSidecar(root *os.Root, id string, local []byte) error {
	name := localFilename(id)
	existing, err := os.ReadFile(filepath.Join(s.dir, name))
	if err == nil {
		if string(existing) == string(local) {
			return nil
		}
	}
	staged, err := root.OpenFile(name+".tmp", os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return err
	}
	if _, err := staged.Write(local); err != nil {
		_ = staged.Close()
		return err
	}
	if err := staged.Sync(); err != nil {
		_ = staged.Close()
		return err
	}
	if err := staged.Close(); err != nil {
		return err
	}
	if err := os.Chmod(filepath.Join(s.dir, name+".tmp"), 0o600); err != nil {
		return err
	}
	return os.Rename(filepath.Join(s.dir, name+".tmp"), filepath.Join(s.dir, name))
}

func (s *Store) writeFileIfMissing(name string, data []byte) {
	path := filepath.Join(s.dir, name)
	if _, err := os.Lstat(path); err == nil {
		return
	}
	_ = os.WriteFile(path, data, 0o600)
}

func (s *Store) syncParent() {
	dir, err := os.Open(s.dir)
	if err != nil {
		return
	}
	defer func() { _ = dir.Close() }()
	_ = dir.Sync()
}

type committedRecord struct {
	id    string
	at    time.Time
	bytes int64
}

func (s *Store) rotate(encoded, local []byte) error {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return err
	}
	now := s.now()
	kept, total := s.scanCommitted(entries, now)
	incoming := int64(len(encoded) + len(local))
	kept = s.evictOverflow(kept, total, incoming)
	s.evictAged(kept, now)
	return nil
}

func (s *Store) scanCommitted(entries []os.DirEntry, now time.Time) ([]committedRecord, int64) {
	var kept []committedRecord
	var total int64
	for _, entry := range entries {
		if !recordName.MatchString(entry.Name()) {
			continue
		}
		if !ownedFile(s.dir, entry.Name()) {
			continue
		}
		id := entry.Name()[len("failure-") : len(entry.Name())-len(".json")]
		info, infoErr := entry.Info()
		if infoErr != nil {
			continue
		}
		kept, total = s.appendCommitted(kept, total, id, info, now)
	}
	slices.SortFunc(kept, func(a, b committedRecord) int {
		if a.at.Equal(b.at) {
			return strings.Compare(a.id, b.id)
		}
		if a.at.Before(b.at) {
			return -1
		}
		return 1
	})
	return kept, total
}

func (s *Store) appendCommitted(kept []committedRecord, total int64, id string, info os.FileInfo, now time.Time) ([]committedRecord, int64) {
	at := info.ModTime()
	if raw, readErr := os.ReadFile(filepath.Join(s.dir, recordFilename(id))); readErr == nil {
		if at.IsZero() {
			at = now
		}
		var decoded Record
		if decodeErr := json.Unmarshal(raw, &decoded); decodeErr == nil && !decoded.Time.IsZero() {
			at = decoded.Time
		}
	}
	size := info.Size()
	if side, sideErr := os.Lstat(filepath.Join(s.dir, localFilename(id))); sideErr == nil {
		size += side.Size()
	}
	return append(kept, committedRecord{id: id, at: at, bytes: size}), total + size
}

func (s *Store) evictOverflow(kept []committedRecord, total, incoming int64) []committedRecord {
	for len(kept) > 0 && (len(kept) >= maxRecords || total+incoming > maxCommitted) {
		oldest := kept[0]
		kept = kept[1:]
		total -= s.removeRecord(oldest.id)
	}
	return kept
}

func (s *Store) evictAged(kept []committedRecord, now time.Time) {
	for len(kept) > 0 && now.Sub(kept[0].at) > maxAge {
		oldest := kept[0]
		kept = kept[1:]
		s.removeRecord(oldest.id)
	}
}

func (s *Store) removeRecord(id string) int64 {
	var freed int64
	for _, name := range []string{recordFilename(id), localFilename(id)} {
		path := filepath.Join(s.dir, name)
		info, err := os.Lstat(path)
		if err != nil {
			continue
		}
		if info.Mode()&fs.ModeSymlink != 0 {
			continue
		}
		if freedSize := info.Size(); freedSize >= 0 {
			freed += freedSize
		}
		_ = os.Remove(path)
	}
	return freed
}

func (s *Store) removeStaging() {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return
	}
	cutoff := s.now().Add(-stagingMaxAge)
	for _, entry := range entries {
		if !strings.HasPrefix(entry.Name(), ".pending-") {
			continue
		}
		info, err := entry.Info()
		if err != nil || info.ModTime().After(cutoff) {
			continue
		}
		_ = os.Remove(filepath.Join(s.dir, entry.Name()))
	}
}

func (s *Store) List(ctx context.Context, query Query) ([]Record, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	s.recordsMu.Lock()
	defer s.recordsMu.Unlock()
	if query.ID != "" {
		return s.findRecord(query.ID)
	}
	return s.listRecords(query.Limit)
}

func (s *Store) findRecord(id string) ([]Record, error) {
	raw, err := os.ReadFile(filepath.Join(s.dir, recordFilename(id)))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, errors.New("failure: record not found")
		}
		return nil, fmt.Errorf("failure: invalid stored record: %w", err)
	}
	var record Record
	if decodeErr := json.Unmarshal(raw, &record); decodeErr != nil {
		return nil, errors.New("failure: invalid stored record")
	}
	return []Record{record}, nil
}

func (s *Store) listRecords(limit int) ([]Record, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return nil, fmt.Errorf("failure: invalid stored record: %w", err)
	}
	var records []Record
	for _, entry := range entries {
		record, skip, entryErr := s.readRecordEntry(entry.Name())
		if entryErr != nil {
			return nil, entryErr
		}
		if skip {
			continue
		}
		records = append(records, record)
	}
	slices.SortFunc(records, func(a, b Record) int {
		if a.Time.Equal(b.Time) {
			return strings.Compare(b.ID, a.ID)
		}
		if a.Time.After(b.Time) {
			return -1
		}
		return 1
	})
	return applyRecordLimit(records, limit), nil
}

func (s *Store) readRecordEntry(name string) (Record, bool, error) {
	if !recordName.MatchString(name) {
		return Record{}, true, nil
	}
	if !ownedFile(s.dir, name) {
		return Record{}, true, nil
	}
	raw, err := os.ReadFile(filepath.Join(s.dir, name))
	if err != nil {
		return Record{}, false, fmt.Errorf("failure: invalid stored record: %w", err)
	}
	var record Record
	if decodeErr := json.Unmarshal(raw, &record); decodeErr != nil {
		return Record{}, false, errors.New("failure: invalid stored record")
	}
	return record, false, nil
}

func applyRecordLimit(records []Record, limit int) []Record {
	if limit <= 0 {
		limit = 20
	}
	if limit > 100 {
		limit = 100
	}
	if len(records) > limit {
		records = records[:limit]
	}
	return records
}

func (s *Store) Summary(ctx context.Context) (Summary, error) {
	if err := ctx.Err(); err != nil {
		return Summary{}, err
	}
	s.recordsMu.Lock()
	defer s.recordsMu.Unlock()
	summary := Summary{}
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		return Summary{}, fmt.Errorf("failure: invalid stored record: %w", err)
	}
	for _, entry := range entries {
		s.accountSummaryEntry(&summary, entry)
	}
	return summary, nil
}

func isSummaryEntry(name string) bool {
	if recordName.MatchString(name) {
		return true
	}
	const prefix = "failure-"
	const suffix = ".local.json"
	if !strings.HasPrefix(name, prefix) || !strings.HasSuffix(name, suffix) {
		return false
	}
	return idPattern.MatchString(name[len(prefix) : len(name)-len(suffix)])
}

func (s *Store) accountSummaryEntry(summary *Summary, entry os.DirEntry) {
	name := entry.Name()
	if !isSummaryEntry(name) {
		return
	}
	if !ownedFile(s.dir, name) {
		return
	}
	info, err := entry.Info()
	if err != nil {
		return
	}
	summary.Bytes += info.Size()
	if !recordName.MatchString(name) {
		return
	}
	summary.Count++
	raw, err := os.ReadFile(filepath.Join(s.dir, name))
	if err != nil {
		return
	}
	var record Record
	if decodeErr := json.Unmarshal(raw, &record); decodeErr != nil {
		return
	}
	if record.Time.After(summary.Latest) {
		summary.Latest = record.Time
	}
}

func (s *Store) Close(_ context.Context) error {
	return nil
}

func (s *Store) Local(ctx context.Context, id string) (LocalRecord, error) {
	if err := ctx.Err(); err != nil {
		return LocalRecord{}, err
	}
	if !idPattern.MatchString(id) {
		return LocalRecord{}, errors.New(ReasonMissingCorrelation)
	}
	s.recordsMu.Lock()
	defer s.recordsMu.Unlock()
	raw, err := os.ReadFile(filepath.Join(s.dir, localFilename(id)))
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return LocalRecord{}, errors.New(ReasonMissingCorrelation)
		}
		return LocalRecord{}, errors.New(ReasonMissingCorrelation)
	}
	var local LocalRecord
	if err := json.Unmarshal(raw, &local); err != nil {
		return LocalRecord{}, errors.New(ReasonMissingLocal)
	}
	if local.SchemaVersion != schemaVersion || local.FailureID != id {
		return LocalRecord{}, errors.New(ReasonMissingLocal)
	}
	return local, nil
}

const ReasonMissingLocal = "failure case unavailable: local metadata is invalid"

func ReportText(record Record) string {
	tool := record.Tool
	if tool == "" {
		tool = "runtime"
	}
	title := "[dalgona-failure:v1:" + record.Fingerprint + "] " + record.Product + " " + record.Version + " " + string(record.Class) + " " + tool
	title = truncateUTF8(title, 240)
	var out strings.Builder
	out.WriteString("Title: " + title + "\n\n")
	out.WriteString("Automated failure report prepared by " + record.Product + ".\n")
	out.WriteString("Fingerprint: " + record.Fingerprint + "\n")
	out.WriteString("Product: " + record.Product + "\n")
	out.WriteString("Version: " + record.Version + "\n")
	out.WriteString("OS: " + record.OS + "\n")
	out.WriteString("Architecture: " + record.Arch + "\n")
	out.WriteString("Failure class: " + string(record.Class) + "\n")
	out.WriteString("Tool: " + tool + "\n")
	out.WriteString("Details withheld: " + boolText(record.ContentFree) + "\n")
	out.WriteString("\nError:\n" + indentBlock(escapeReportText(record.Error, false)) + "\n")
	out.WriteString("\nStack:\n" + indentBlock(stackText(record.Stack)) + "\n")
	out.WriteString("\nTool arguments excerpt:\n" + indentBlock(escapeReportText(record.Args, false)) + "\n")
	out.WriteString("\nTool output excerpt:\n" + indentBlock(escapeReportText(record.Output, false)) + "\n")
	out.WriteString("\nFailing-step attachment (JSON):\n" + indentBlock(attachmentText(record.FailingStep)) + "\n")
	body := out.String()
	if len(body) <= 32768 {
		return body
	}
	for _, shrink := range []func(Record) Record{withoutOutput, withoutArgs, withoutError} {
		body = renderReport(shrink(record))
		if len(body) <= 32768 {
			return body
		}
	}
	return body
}

func renderReport(record Record) string {
	tool := record.Tool
	if tool == "" {
		tool = "runtime"
	}
	title := truncateUTF8("[dalgona-failure:v1:"+record.Fingerprint+"] "+record.Product+" "+record.Version+" "+string(record.Class)+" "+tool, 240)
	var out strings.Builder
	out.WriteString("Title: " + title + "\n\n")
	out.WriteString("Automated failure report prepared by " + record.Product + ".\n")
	out.WriteString("Fingerprint: " + record.Fingerprint + "\n")
	out.WriteString("Product: " + record.Product + "\n")
	out.WriteString("Version: " + record.Version + "\n")
	out.WriteString("OS: " + record.OS + "\n")
	out.WriteString("Architecture: " + record.Arch + "\n")
	out.WriteString("Failure class: " + string(record.Class) + "\n")
	out.WriteString("Tool: " + tool + "\n")
	out.WriteString("Details withheld: " + boolText(record.ContentFree) + "\n")
	out.WriteString("\nError:\n" + indentBlock(escapeReportText(record.Error, false)) + "\n")
	out.WriteString("\nStack:\n" + indentBlock(stackText(record.Stack)) + "\n")
	out.WriteString("\nTool arguments excerpt:\n" + indentBlock(escapeReportText(record.Args, false)) + "\n")
	out.WriteString("\nTool output excerpt:\n" + indentBlock(escapeReportText(record.Output, false)) + "\n")
	out.WriteString("\nFailing-step attachment (JSON):\n" + indentBlock(attachmentText(record.FailingStep)) + "\n")
	return out.String()
}

func withoutOutput(record Record) Record {
	record.Output = ""
	return record
}

func withoutArgs(record Record) Record {
	record.Args = ""
	return record
}

func withoutError(record Record) Record {
	record.Error = ""
	return record
}

func boolText(value bool) string {
	if value {
		return "true"
	}
	return "false"
}

func indentBlock(text string) string {
	if text == "" {
		return "    (none)"
	}
	lines := strings.Split(text, "\n")
	for i, line := range lines {
		lines[i] = "    " + line
	}
	return strings.Join(lines, "\n")
}

func stackText(frames []Frame) string {
	if len(frames) == 0 {
		return "(none)"
	}
	lines := make([]string, 0, len(frames))
	for _, frame := range frames {
		lines = append(lines, escapeReportText(frame.Function+"@"+frame.File+":"+frameLine(frame.Line), true))
	}
	return strings.Join(lines, "\n")
}

func frameLine(line int) string {
	if line < 0 {
		return "0"
	}
	return strconv.Itoa(line)
}

func attachmentText(step *FailureStep) string {
	target := step
	if target == nil {
		envelope := UnavailableStep(ReasonMissingCompletion)
		target = &envelope
	}
	encoded, err := json.Marshal(target)
	if err != nil {
		envelope := UnavailableStep(ReasonMissingCompletion)
		encoded, _ = json.Marshal(envelope)
	}
	return escapeReportText(string(encoded), false)
}

func escapeReportText(text string, stackLine bool) string {
	if text == "" {
		return ""
	}
	var out strings.Builder
	for _, c := range text {
		writeReportRune(&out, c, stackLine)
	}
	return out.String()
}

func writeReportRune(out *strings.Builder, c rune, stackLine bool) {
	if c == '\n' || c == '\t' {
		out.WriteRune(c)
		return
	}
	if c < 0x20 || c == 0x7f {
		out.WriteRune('�')
		return
	}
	if entity, ok := reportEntity(c, stackLine); ok {
		out.WriteString(entity)
		return
	}
	out.WriteRune(c)
}

func reportEntity(c rune, stackLine bool) (string, bool) {
	switch c {
	case '&':
		return "&amp;", true
	case '<':
		return "&lt;", true
	case '>':
		return "&gt;", true
	case '@':
		if stackLine {
			return "", false
		}
		return "@\u200b", true
	default:
		return "", false
	}
}
