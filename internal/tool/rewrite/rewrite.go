package rewrite

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/syntax"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/patch"
	"github.com/gosuda/dalgona/internal/tool/search"
)

const (
	rewriteMaxFileBytes = 1 << 20
	rewriteMaxScanned   = 256
	rewriteMaxFiles     = 32
	rewriteMaxEdits     = 256
	rewriteDeadline     = 90 * time.Second
)

var errRewriteDeadline = errors.New("rewrite: deadline reached")

const description = `Rewrite code with a tree-sitter query. lang names the grammar. pattern is a tree-sitter query, such as (function_declaration name: (identifier) @name). target names the capture whose text is replaced, without @. replace is the new text. @name inserts the text of that capture in the same match. @{name} does the same for a name with dots, such as @{definition.function}. @@ writes a literal @. path and glob select files as search does. Replacement bytes are spliced as written, so the tool does not convert line endings. The call writes nothing and refuses when the query does not compile, when a file has syntax errors before or after the call, when two targets overlap, or when a limit is exceeded. A replacement that still matches the query is allowed, and a second call changes the file again. Preview the matches with search, mode syntax, and the same lang, pattern, path and glob. Edits go through the same approval, journal and review as patch.`

const schema = `{"type":"object","properties":{"lang":{"type":"string","enum":["bash","c","c_sharp","cpp","css","go","html","java","javascript","json","kotlin","lua","markdown","php","python","ruby","rust","svelte","swift","toml","tsx","typescript","yaml"]},"pattern":{"type":"string"},"target":{"type":"string"},"replace":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"}},"required":["lang","pattern","target","replace"],"additionalProperties":false}`

type Editor struct{}

func New() tool.Editor { return Editor{} }

func (Editor) Spec() tool.Spec {
	return tool.Spec{Name: "rewrite", Description: description, Schema: jsontext.Value(schema), Keep: tool.KeepHead}
}

func (Editor) Class() tool.Class { return tool.ClassEdit }

func (e Editor) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	plan, err := e.Plan(ctx, call, env)
	if err != nil {
		return tool.Result{}, err
	}
	return plan.Apply(ctx)
}

type args struct {
	Lang    string `json:"lang"`
	Pattern string `json:"pattern"`
	Target  string `json:"target"`
	Replace string `json:"replace"`
	Path    string `json:"path"`
	Glob    string `json:"glob"`
}

type query struct {
	lang  syntax.Language
	query *syntax.Query
	tmpl  template
}

type changed struct {
	path  string
	src   []byte
	post  []byte
	edits []edit
}

type scanner struct {
	env    tool.Env
	parser *syntax.Parser
	spec   query
	target string
}

func (Editor) Plan(ctx context.Context, call tool.Call, env tool.Env) (tool.Plan, error) {
	var a args
	if err := json.Unmarshal(call.Args, &a, json.RejectUnknownMembers(true)); err != nil {
		return tool.Plan{}, fmt.Errorf("rewrite: invalid arguments: %w", err)
	}
	shown := a.Pattern
	pattern, replace, err := unmaskArgs(env, a.Pattern, a.Replace)
	if err != nil {
		return tool.Plan{}, err
	}
	spec, err := prepare(a, pattern, replace)
	if err != nil {
		return tool.Plan{}, err
	}
	if !tool.IsLocalRunner(env.Runner) {
		return tool.Plan{}, errors.New("rewrite: the rewrite tool needs the local runner; this runner is not local")
	}
	ctx, cancel := context.WithTimeoutCause(ctx, rewriteDeadline, errRewriteDeadline)
	defer cancel()
	files, err := collect(ctx, env, a, spec)
	if err != nil {
		return tool.Plan{}, err
	}
	if len(files) == 0 {
		return noMatch(shown), nil
	}
	if err = checkLimits(files); err != nil {
		return tool.Plan{}, err
	}
	summary, replaced := summarise(files)
	return patch.PlanReplace(ctx, env, summary, replaced)
}

func unmaskArgs(env tool.Env, pattern, replace string) (string, string, error) {
	if env.Masker == nil {
		return pattern, replace, nil
	}
	restoredPattern, err := env.Masker.UnmaskStrict(pattern)
	if err != nil {
		return "", "", fmt.Errorf("rewrite: secret placeholder cannot be restored: %w", err)
	}
	restoredReplace, err := env.Masker.UnmaskStrict(replace)
	if err != nil {
		return "", "", fmt.Errorf("rewrite: secret placeholder cannot be restored: %w", err)
	}
	return restoredPattern, restoredReplace, nil
}

func prepare(a args, pattern, replace string) (query, error) {
	lang, ok := syntax.ByName(a.Lang)
	if !ok {
		return query{}, fmt.Errorf("rewrite: unknown lang %s, use one of: %s", a.Lang, strings.Join(syntax.Names(), ", "))
	}
	if pattern == "" {
		return query{}, errors.New("rewrite: pattern must not be empty")
	}
	if a.Target == "" {
		return query{}, errors.New("rewrite: target must not be empty")
	}
	q, err := lang.Compile(pattern)
	if err != nil {
		return query{}, fmt.Errorf("rewrite: %w", err)
	}
	names := q.CaptureNames()
	if !slices.Contains(names, a.Target) {
		return query{}, fmt.Errorf("rewrite: target %s is not a capture of the query", a.Target)
	}
	tmpl, err := parseTemplate(replace)
	if err != nil {
		return query{}, err
	}
	for _, name := range tmpl.captures() {
		if !slices.Contains(names, name) {
			return query{}, fmt.Errorf("rewrite: replace names @%s, which the query does not capture", name)
		}
	}
	return query{lang: lang, query: q, tmpl: tmpl}, nil
}

func collect(ctx context.Context, env tool.Env, a args, spec query) ([]changed, error) {
	paths, err := search.Select(ctx, env.Cwd, a.Path, a.Glob, spec.lang.Extensions())
	if err != nil {
		return nil, cancelled(ctx, err)
	}
	if len(paths) > rewriteMaxScanned {
		return nil, fmt.Errorf("rewrite: %d files match; the limit is %d. Narrow path or glob.", len(paths), rewriteMaxScanned) //nolint:staticcheck // the planned refusal keeps its final period
	}
	sc := scanner{env: env, parser: spec.lang.NewParser(), spec: spec, target: a.Target}
	var out []changed
	for _, path := range paths {
		if ctx.Err() != nil {
			return nil, context.Cause(ctx)
		}
		item, ok, scanErr := sc.file(ctx, path)
		if scanErr != nil {
			return nil, scanErr
		}
		if ok {
			out = append(out, item)
		}
	}
	return out, nil
}

func (sc scanner) file(ctx context.Context, path string) (changed, bool, error) {
	src, err := sc.read(ctx, path)
	if err != nil {
		return changed{}, false, err
	}
	caps, err := sc.parse(ctx, path, src)
	if err != nil {
		return changed{}, false, err
	}
	groups, err := groupMatches(caps)
	if err != nil {
		return changed{}, false, err
	}
	edits, err := editsOf(path, src, groups, sc.target, sc.spec.tmpl)
	if err != nil || len(edits) == 0 {
		return changed{}, false, err
	}
	post := apply(src, edits)
	if bytes.Equal(post, src) {
		return changed{}, false, nil
	}
	return sc.check(ctx, path, src, post, edits)
}

func (sc scanner) read(ctx context.Context, path string) ([]byte, error) {
	absolute := filepath.Join(sc.env.Cwd, filepath.FromSlash(path))
	info, err := sc.env.Runner.Stat(ctx, absolute)
	if err != nil {
		return nil, cancelled(ctx, err)
	}
	if info.Size > rewriteMaxFileBytes {
		return nil, fmt.Errorf("rewrite: %s is over %d bytes; narrow path or glob.", path, rewriteMaxFileBytes) //nolint:staticcheck // the planned refusal keeps its final period
	}
	src, err := sc.env.Runner.ReadFile(ctx, absolute)
	if err != nil {
		return nil, cancelled(ctx, err)
	}
	return src, nil
}

func (sc scanner) parse(ctx context.Context, path string, src []byte) ([]syntax.Capture, error) {
	caps, report, err := sc.parser.Captures(ctx, sc.spec.query, src)
	if err != nil {
		return nil, cancelled(ctx, err)
	}
	if err = checkStopped(path, report); err != nil {
		return nil, err
	}
	if report.SyntaxErrors {
		return nil, fmt.Errorf("rewrite: %s has syntax errors; fix it with patch first", path)
	}
	return caps, nil
}

func (sc scanner) check(ctx context.Context, path string, src, post []byte, edits []edit) (changed, bool, error) {
	_, after, err := sc.parser.Captures(ctx, sc.spec.query, post)
	if err != nil {
		return changed{}, false, cancelled(ctx, err)
	}
	if err = checkStopped(path, after); err != nil {
		return changed{}, false, err
	}
	if after.SyntaxErrors {
		return changed{}, false, fmt.Errorf("rewrite: %s has a syntax error at line %d after the rewrite; nothing was written.", path, after.ErrorLine) //nolint:staticcheck // the planned refusal keeps its final period
	}
	return changed{path: path, src: src, post: post, edits: edits}, true, nil
}

func checkStopped(path string, report syntax.Report) error {
	if report.Stopped {
		return fmt.Errorf("rewrite: parse stopped for %s; nothing was written.", path) //nolint:staticcheck // the planned refusal keeps its final period
	}
	return nil
}

func checkLimits(files []changed) error {
	if len(files) > rewriteMaxFiles {
		return fmt.Errorf("rewrite: %d files would change; the limit is %d. Narrow path or glob.", len(files), rewriteMaxFiles) //nolint:staticcheck // the planned refusal keeps its final period
	}
	total := 0
	for _, file := range files {
		total += len(file.edits)
	}
	if total > rewriteMaxEdits {
		return fmt.Errorf("rewrite: %d replacements would change; the limit is %d. Narrow path or glob.", total, rewriteMaxEdits) //nolint:staticcheck // the planned refusal keeps its final period
	}
	return nil
}

func summarise(files []changed) (string, []patch.ReplacedFile) {
	var summary strings.Builder
	total := 0
	replaced := make([]patch.ReplacedFile, 0, len(files))
	for _, file := range files {
		total += len(file.edits)
	}
	fmt.Fprintf(&summary, "rewrite: %d replacements in %d files\n", total, len(files))
	for _, file := range files {
		fmt.Fprintf(&summary, "%s: %d replacements\n", file.path, len(file.edits))
		replaced = append(replaced, patch.ReplacedFile{Path: file.path, Before: file.src, After: file.post, Splices: splicesOf(file.edits)})
	}
	return summary.String(), replaced
}

func noMatch(shown string) tool.Plan {
	return tool.Plan{
		Apply: func(context.Context) (tool.Result, error) {
			return tool.Result{Output: []byte("No matches for " + shown), OK: true}, nil
		},
		Discard: func() {},
	}
}

func cancelled(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return context.Cause(ctx)
	}
	return err
}
