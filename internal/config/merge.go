package config

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strings"
	"time"

	"github.com/pelletier/go-toml/v2"
	"github.com/pelletier/go-toml/v2/unstable"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/keywords"
)

type fileDoc struct {
	Model                *string                `toml:"model"`
	Reasoning            *string                `toml:"reasoning"`
	ApprovalValue        *string                `toml:"approval"`
	ToolMode             *string                `toml:"tool_mode"`
	Sandbox              *string                `toml:"sandbox"`
	Verbosity            *string                `toml:"verbosity"`
	Theme                *string                `toml:"theme"`
	Icons                *string                `toml:"icons"`
	StatusHide           *[]string              `toml:"status_hide"`
	Notifications        *bool                  `toml:"notifications"`
	SandboxWritableRoots []string               `toml:"sandbox_writable_roots"`
	EditStyle            editStyleDoc           `toml:"edit_style"`
	CompactionThreshold  *float64               `toml:"compaction_threshold"`
	CompactionKeepTokens *int                   `toml:"compaction_keep_tokens"`
	StreamStartTimeout   *string                `toml:"stream_start_timeout"`
	StreamIdleTimeout    *string                `toml:"stream_idle_timeout"`
	Runner               *string                `toml:"runner"`
	Remote               *string                `toml:"remote"`
	Providers            map[string]providerDoc `toml:"providers"`
	Models               map[string]modelDoc    `toml:"models"`
	Runners              map[string]runnerDoc   `toml:"runners"`
	Remotes              map[string]remoteDoc   `toml:"remotes"`
	MCPServers           map[string]mcpDoc      `toml:"mcp_servers"`
	MCPTrustedWorkspaces []string               `toml:"mcp_trusted_workspaces"`
	Discovery            discoveryDoc           `toml:"discovery"`
	ApprovalDecider      approvalDeciderDoc     `toml:"approval_decider"`
	EditReview           editReviewDoc          `toml:"edit_review"`
	Eval                 *evalDoc               `toml:"eval"`
	Secrets              secretsDoc             `toml:"secrets"`
	Keywords             map[string]keywordDoc  `toml:"keywords"`
	Herdr                herdrDoc               `toml:"herdr"`
	Web                  webDoc                 `toml:"web"`
	Roles                rolesDoc               `toml:"roles"`
	Advisor              advisorDoc             `toml:"advisor"`
	Advisors             map[string]advisorsDoc `toml:"advisors"`
}

type editStyleDoc struct {
	Spec EditStyleSpec
	Set  bool
}

func (d *editStyleDoc) UnmarshalTOML(raw []byte) error {
	raw = bytes.TrimSpace(raw)
	var scalar struct {
		Value string `toml:"value"`
	}
	scalarRaw := append([]byte("value = "), raw...)
	if err := toml.Unmarshal(scalarRaw, &scalar); err == nil {
		d.Spec = EditStyleSpec{Default: scalar.Value}
		d.Set = true
		return nil
	}
	styles, err := decodeEditStyleTable(raw)
	if err != nil {
		return err
	}
	keys, err := orderedEditStyleKeys(raw)
	if err != nil {
		return fmt.Errorf("edit_style order: %w", err)
	}
	d.Spec = editStyleFromKeys(keys, styles)
	d.Set = true
	return nil
}

func decodeEditStyleTable(raw []byte) (map[string]string, error) {
	if len(raw) > 0 && raw[0] == '{' {
		var inline struct {
			Value map[string]string `toml:"value"`
		}
		if err := toml.Unmarshal(append([]byte("value = "), raw...), &inline); err != nil {
			return nil, fmt.Errorf("edit_style inline table: %w", err)
		}
		return inline.Value, nil
	}
	var styles map[string]string
	if err := toml.Unmarshal(raw, &styles); err != nil {
		return nil, fmt.Errorf("edit_style table: %w", err)
	}
	return styles, nil
}

func editStyleFromKeys(keys []string, styles map[string]string) EditStyleSpec {
	spec := EditStyleSpec{Rows: make([]EditStyleRow, 0, len(keys))}
	for _, key := range keys {
		if key == "default" {
			spec.Default = styles[key]
			continue
		}
		spec.Rows = append(spec.Rows, EditStyleRow{Glob: key, Style: styles[key]})
	}
	return spec
}

func orderedEditStyleKeys(raw []byte) ([]string, error) {
	if len(raw) > 0 && raw[0] == '{' {
		raw = append([]byte("edit_style = "), raw...)
	}
	var parser unstable.Parser
	parser.Reset(raw)
	var keys []string
	for parser.NextExpression() {
		expression := parser.Expression()
		if expression.Kind != unstable.KeyValue {
			continue
		}
		if expression.Value().Kind == unstable.InlineTable {
			children := expression.Value().Children()
			for children.Next() {
				keys = append(keys, tomlKey(children.Node()))
			}
			continue
		}
		keys = append(keys, tomlKey(expression))
	}
	if err := parser.Error(); err != nil {
		return nil, fmt.Errorf("parse edit_style: %w", err)
	}
	return keys, nil
}

func tomlKey(node *unstable.Node) string {
	parts := node.Key()
	var key strings.Builder
	for parts.Next() {
		if key.Len() > 0 {
			key.WriteByte('.')
		}
		key.Write(parts.Node().Data)
	}
	return key.String()
}

type (
	providerDoc struct {
		BaseURL   string `toml:"base_url"`
		APIKeyEnv string `toml:"api_key_env"`
	}
	modelDoc struct {
		ContextWindow *int `toml:"context_window"`
	}
	runnerDoc struct {
		Service  string `toml:"service"`
		Image    string `toml:"image"`
		Template string `toml:"template"`
		URL      string `toml:"url"`
	}
	remoteDoc struct {
		Service  *string `toml:"service"`
		Image    *string `toml:"image"`
		Template *string `toml:"template"`
		URL      *string `toml:"url"`
		BoxID    *string `toml:"box_id"`
	}
	mcpDoc struct {
		Command string            `toml:"command"`
		Args    []string          `toml:"args"`
		Env     map[string]string `toml:"env"`
		URL     string            `toml:"url"`
	}
	discoveryDoc struct {
		Sources []string `toml:"sources"`
	}
	approvalDeciderDoc struct {
		Decider   *string  `toml:"decider"`
		Threshold *float64 `toml:"threshold"`
	}
	editReviewDoc struct {
		Enabled  *bool    `toml:"enabled"`
		Feedback []string `toml:"feedback"`
	}
	evalDoc struct {
		Uses []string `toml:"uses"`
	}
	secretsDoc struct {
		Patterns []string `toml:"patterns"`
	}
	keywordDoc struct {
		Notice   string   `toml:"notice"`
		Text     string   `toml:"text"`
		Hue      []int    `toml:"hue"`
		Effort   string   `toml:"effort"`
		Requires []string `toml:"requires"`
		Enabled  *bool    `toml:"enabled"`
	}
	herdrDoc struct {
		SubagentPanes *bool `toml:"subagent_panes"`
	}
	webDoc struct {
		Listen   *string `toml:"listen"`
		Token    *string `toml:"token"`
		CertFile *string `toml:"cert_file"`
		KeyFile  *string `toml:"key_file"`
	}
	rolesDoc struct {
		Smol    *string `toml:"smol"`
		Slow    *string `toml:"slow"`
		Advisor *string `toml:"advisor"`
	}
)

func (c *Config) merge(current layer) error {
	if err := c.mergeScalars(current); err != nil {
		return err
	}
	c.mergeMaps(current.doc)
	c.mergeTables(current.doc)
	c.mergeAdvisor(current.doc)
	for key, location := range lineMap(current.path, current.raw) {
		c.lines[key] = location
	}
	return nil
}

func (c *Config) mergeScalars(current layer) error {
	doc := current.doc
	setString(doc.Model, &c.Model)
	setString(doc.Reasoning, (*string)(&c.Reasoning))
	setString(doc.ApprovalValue, (*string)(&c.Approval))
	setString(doc.ToolMode, (*string)(&c.ToolMode))
	setString(doc.Sandbox, (*string)(&c.Sandbox))
	setString(doc.Verbosity, (*string)(&c.Verbosity))
	setString(doc.Theme, (*string)(&c.Theme))
	setString(doc.Icons, (*string)(&c.Icons))
	setStrings(doc.StatusHide, &c.StatusHide)
	setBool(doc.Notifications, &c.Notifications)
	setString(doc.Runner, &c.Runner)
	setString(doc.Remote, &c.Remote)
	if doc.SandboxWritableRoots != nil {
		c.SandboxWritableRoots = doc.SandboxWritableRoots
	}
	if doc.EditStyle.Set {
		c.EditStyle = doc.EditStyle.Spec
	}
	if doc.CompactionThreshold != nil {
		c.CompactionThreshold = *doc.CompactionThreshold
	}
	if doc.CompactionKeepTokens != nil {
		c.CompactionKeepTokens = *doc.CompactionKeepTokens
	}
	if err := c.mergeDurations(current); err != nil {
		return err
	}
	return nil
}

func setString(source *string, target *string) {
	if source != nil {
		*target = *source
	}
}

func setStrings(source *[]string, target *[]string) {
	if source != nil {
		*target = *source
	}
}

func (c *Config) mergeDurations(current layer) error {
	for _, item := range []struct {
		key    string
		raw    *string
		target *time.Duration
	}{
		{key: "stream_start_timeout", raw: current.doc.StreamStartTimeout, target: &c.StreamStartTimeout},
		{key: "stream_idle_timeout", raw: current.doc.StreamIdleTimeout, target: &c.StreamIdleTimeout},
	} {
		if item.raw == nil {
			continue
		}
		value, err := time.ParseDuration(*item.raw)
		if err != nil {
			location := lineLocation(current.path, current.raw, item.key)
			return fmt.Errorf("%s:%d: %s: %w", location.file, location.line, item.key, err)
		}
		*item.target = value
	}
	return nil
}

func (c *Config) mergeMaps(doc *fileDoc) {
	for name, value := range doc.Providers {
		c.Providers[name] = ProviderSpec(value)
	}
	for name, value := range doc.Models {
		model := ModelSpec{}
		if value.ContextWindow != nil {
			model.ContextWindow = *value.ContextWindow
		}
		c.Models[name] = model
	}
	c.mergeBoxes(doc)
	for name, value := range doc.MCPServers {
		c.MCPServers[name] = MCPServerSpec(value)
	}
	for word, value := range doc.Keywords {
		keyword := KeywordSpec{Notice: value.Notice, Text: value.Text, Effort: value.Effort, Requires: value.Requires}
		if len(value.Hue) == 2 {
			keyword.Hue = [2]int{value.Hue[0], value.Hue[1]}
		}
		if value.Enabled != nil {
			keyword.Enabled = *value.Enabled
		} else if existing, ok := c.Keywords[word]; ok {
			keyword.Enabled = existing.Enabled
		} else {
			keyword.Enabled = keywords.DefaultEnabled(string(c.Product), word)
		}
		c.Keywords[word] = keyword
	}
}

func (c *Config) mergeBoxes(doc *fileDoc) {
	for name, value := range doc.Runners {
		c.Runners[name] = RunnerSpec(value)
	}
	for name, value := range doc.Remotes {
		spec := c.Remotes[name]
		setString(value.Service, &spec.Service)
		setString(value.Image, &spec.Image)
		setString(value.Template, &spec.Template)
		setString(value.URL, &spec.URL)
		setString(value.BoxID, &spec.BoxID)
		c.Remotes[name] = spec
	}
}

func (c *Config) mergeTables(doc *fileDoc) {
	if doc.Discovery.Sources != nil {
		c.Discovery.Sources = doc.Discovery.Sources
	}
	setString(doc.ApprovalDecider.Decider, &c.ApprovalDecider.Decider)
	if doc.ApprovalDecider.Threshold != nil {
		c.ApprovalDecider.Threshold = *doc.ApprovalDecider.Threshold
	}
	if doc.EditReview.Enabled != nil {
		c.EditReview.Enabled = *doc.EditReview.Enabled
	}
	if doc.EditReview.Feedback != nil {
		c.EditReview.Feedback = doc.EditReview.Feedback
	}
	if doc.Eval != nil {
		c.Eval = EvalConfig{Uses: doc.Eval.Uses, Set: true}
	}
	if doc.Secrets.Patterns != nil {
		c.Secrets.Patterns = doc.Secrets.Patterns
	}
	if doc.Herdr.SubagentPanes != nil {
		c.Herdr.SubagentPanes = *doc.Herdr.SubagentPanes
	}
	mergeWeb(&c.Web, doc.Web)
	setString(doc.Roles.Smol, &c.Roles.Smol)
	setString(doc.Roles.Slow, &c.Roles.Slow)
	setString(doc.Roles.Advisor, &c.Roles.Advisor)
}

func mergeWeb(target *WebConfig, source webDoc) {
	setString(source.Listen, &target.Listen)
	setString(source.Token, &target.Token)
	setString(source.CertFile, &target.CertFile)
	setString(source.KeyFile, &target.KeyFile)
}

func (c *Config) applyFlags(flags Flags) {
	c.setFlag(flags.Model, &c.Model, "model")
	c.setFlag(flags.Reasoning, (*string)(&c.Reasoning), "reasoning")
	c.setFlag(flags.Approval, (*string)(&c.Approval), "approval")
	c.setFlag(flags.ToolMode, (*string)(&c.ToolMode), "tool_mode")
	c.setFlag(flags.Sandbox, (*string)(&c.Sandbox), "sandbox")
	c.setFlag(flags.Runner, &c.Runner, "runner")
	c.setFlag(flags.Remote, &c.Remote, "remote")
	c.setFlag(flags.Cwd, &c.Cwd, "cwd")
	if flags.Advisor {
		c.Advisor.Enabled = true
	}
}

func (c *Config) setFlag(source string, target *string, key string) {
	if source == "" {
		return
	}
	*target = source
	c.lines[key] = origin{file: "flags", line: 0}
}

type exactError string

func (e exactError) Error() string { return string(e) }

func (c *Config) validate() error {
	var errs []error
	errs = append(errs, c.validateEnums()...)
	errs = append(errs, c.validateApprovalDecider()...)
	errs = append(errs, c.validateEditStyle()...)
	errs = append(errs, c.validateRunners()...)
	errs = append(errs, c.validateKeywords()...)
	errs = append(errs, c.validateAdvisors()...)
	errs = append(errs, c.validateStatusHideList()...)
	return errors.Join(errs...)
}

func (c *Config) validateEnums() []error {
	var errs []error
	if !one(string(c.Approval), "auto", "yolo", "ask") {
		errs = append(errs, c.valueError("approval", "approval must be one of auto, yolo, ask"))
	}
	if !one(string(c.Sandbox), "workspace-write", "off") {
		errs = append(errs, c.valueError("sandbox", "sandbox must be one of workspace-write, off"))
	}
	if !one(string(c.Reasoning), "default", "low", "medium", "high", "xhigh", "max") {
		errs = append(errs, c.valueError("reasoning", "reasoning must be one of default, low, medium, high, xhigh, max"))
	}
	if !one(string(c.ToolMode), "normal", "eval-first", "eval-only") {
		errs = append(errs, c.valueError("tool_mode", "tool_mode must be one of normal, eval-first, eval-only"))
	}
	for _, choice := range []struct{ key, value string }{{"verbosity", string(c.Verbosity)}, {"theme", string(c.Theme)}, {"icons", string(c.Icons)}} {
		options := optionsOf(choice.key)
		if !one(choice.value, options...) {
			errs = append(errs, c.valueError(choice.key, choice.key+" must be one of "+strings.Join(options, ", ")))
		}
	}
	return errs
}

func (c *Config) validateStatusHideList() []error {
	if err := validateStatusHide(c.StatusHide); err != nil {
		return []error{c.valueError("status_hide", err.Error())}
	}
	return nil
}

func (c *Config) validateApprovalDecider() []error {
	var errs []error
	if c.ApprovalDecider.Decider != "" && !strings.HasPrefix(c.ApprovalDecider.Decider, "decisions:") && !strings.HasPrefix(c.ApprovalDecider.Decider, "llm:") {
		errs = append(errs, c.valueError("approval_decider.decider", "[approval_decider] decider must start with decisions: or llm:"))
	}
	if c.ApprovalDecider.Threshold <= 0 || c.ApprovalDecider.Threshold > 1 {
		errs = append(errs, c.valueError("approval_decider.threshold", "[approval_decider] threshold must be in (0, 1]"))
	}
	return errs
}

func (c *Config) validateEditStyle() []error {
	if c.EditStyle.Default == "" {
		return []error{exactError(c.originFile("edit_style") + ": edit_style table needs a default entry.")}
	}
	if !validEditStyle(c.EditStyle.Default) {
		return []error{exactError(c.originFile("edit_style") + ": edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict.")}
	}
	for _, row := range c.EditStyle.Rows {
		if row.Glob == "" {
			return []error{exactError(c.originFile("edit_style") + ": edit_style keys must not be empty.")}
		}
		if !validEditStyle(row.Style) {
			return []error{exactError(c.originFile("edit_style") + ": edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict.")}
		}
	}
	return nil
}

func validEditStyle(style string) bool {
	return one(style, "anchor", "replace", "hashline", "hashline-light", "hashline-enhanced", "apply_patch", "simple", "balanced", "strict")
}

func (c *Config) validateRunners() []error {
	var errs []error
	for _, name := range slices.Sorted(maps.Keys(c.Runners)) {
		runner := c.Runners[name]
		if runner.Service != "sandbox0" {
			continue
		}
		key := "runners." + name
		if runner.Image != "" {
			errs = append(errs, fmt.Errorf("%s: runners.%s: sandbox0 takes template, not image", c.originFile(key), name))
		} else if runner.Template == "" {
			errs = append(errs, fmt.Errorf("%s: runners.%s: sandbox0 requires a template", c.originFile(key), name))
		}
	}
	return errs
}

func (c *Config) validateKeywords() []error {
	var errs []error
	for _, word := range slices.Sorted(maps.Keys(c.Keywords)) {
		if err := validateKeyword(c.Home, c.originFile("keywords."+word), word, c.Keywords[word]); err != nil {
			errs = append(errs, err)
		}
	}
	return errs
}

func (c *Config) originFile(key string) string {
	if location, ok := c.lines[key]; ok {
		return location.file
	}
	return "config.toml"
}

func (c *Config) valueError(key, message string) error {
	location, ok := c.lines[key]
	if !ok {
		location = origin{file: "config.toml", line: 1}
	}
	if location.line == 0 {
		return fmt.Errorf("%s: %s", location.file, message)
	}
	return fmt.Errorf("%s:%d: %s", location.file, location.line, message)
}

func one(value string, allowed ...string) bool {
	for _, candidate := range allowed {
		if value == candidate {
			return true
		}
	}
	return false
}

var keywordWordRe = regexp.MustCompile(`^[a-z][a-z0-9_-]{1,31}$`)

var builtinKeywords = map[string]bool{"ultrathink": true, "jevify": true, "workflowz": true, "orchestrate": true}

func validateKeyword(home, originFile, word string, keyword KeywordSpec) error {
	if !keywordWordRe.MatchString(word) {
		return fmt.Errorf("%s: keyword %q must be lowercase letters, digits, _ or -, 2-32 chars", originFile, word)
	}
	if !builtinKeywords[word] && (keyword.Notice == "") == (keyword.Text == "") {
		return fmt.Errorf("%s: keyword %q needs exactly one of notice or text", originFile, word)
	}
	if builtinKeywords[word] && keyword.Notice != "" && keyword.Text != "" {
		return fmt.Errorf("%s: keyword %q needs exactly one of notice or text", originFile, word)
	}
	if keyword.Notice != "" && !keywordNoticeExists(home, keyword.Notice) {
		return fmt.Errorf("%s: keyword %q notice file %s not found", originFile, word, keyword.Notice)
	}
	return nil
}

func keywordNoticeExists(home, notice string) bool {
	clean := filepath.Clean(notice)
	if filepath.IsAbs(notice) || clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return false
	}
	info, err := os.Stat(filepath.Join(home, clean))
	return err == nil && info.Mode().IsRegular()
}

var dalgonaOnlyKeys = map[string]bool{"tool_mode": true, "runner": true, "remote": true, "runners": true, "remotes": true, "mcp_servers": true, "mcp_trusted_workspaces": true, "discovery": true, "eval": true, "herdr": true, "web": true, "edit_review": true}

func validateProductLayer(product Product, current layer) error {
	if product != ProductDal {
		return nil
	}
	var found *origin
	foundKey := ""
	for key, location := range lineMap(current.path, current.raw) {
		root := strings.SplitN(key, ".", 2)[0]
		if dalgonaOnlyKeys[root] && (found == nil || location.line < found.line) {
			found = &location
			foundKey = root
		}
	}
	if found == nil {
		return nil
	}
	return fmt.Errorf("%s:%d: %s is a dalgona setting", found.file, found.line, foundKey)
}

func lineLocation(path string, raw []byte, key string) origin {
	return lineMap(path, raw)[key]
}

func lineMap(path string, raw []byte) map[string]origin {
	locations := map[string]origin{}
	section := ""
	var parser unstable.Parser
	parser.Reset(raw)
	for parser.NextExpression() {
		expression := parser.Expression()
		if expression.Kind == unstable.Table || expression.Kind == unstable.ArrayTable {
			section = tomlKey(expression)
			locations[section] = origin{file: filepath.Base(path), line: tomlKeyLine(raw, expression)}
			continue
		}
		if expression.Kind == unstable.KeyValue {
			key := tomlKey(expression)
			if section != "" {
				key = section + "." + key
			}
			locations[key] = origin{file: filepath.Base(path), line: tomlKeyLine(raw, expression)}
		}
	}
	return locations
}

func tomlKeyLine(raw []byte, node *unstable.Node) int {
	offset := node.Raw.Offset
	parts := node.Key()
	if parts.Next() {
		offset = parts.Node().Raw.Offset
	}
	return 1 + bytes.Count(raw[:offset], []byte{'\n'})
}

var ErrSettingsChanged = errors.New("settings.toml changed")

func WriteSettings(home string, edits ...SettingEdit) error {
	_, err := WriteSettingsIf(home, "", edits...)
	return err
}

func WriteSettingsIf(home, expectedVersion string, edits ...SettingEdit) (string, error) {
	unlock, err := auth.LockHome(home)
	if err != nil {
		return "", fmt.Errorf("lock settings write: %w", err)
	}
	defer func() { _ = unlock() }()
	path := filepath.Join(home, "settings.toml")
	raw, err := readSettingsFile(path)
	if err != nil {
		return "", err
	}
	if expectedVersion != "" && digestSettings(raw) != expectedVersion {
		return "", fmt.Errorf("%w; expected version %s", ErrSettingsChanged, expectedVersion)
	}
	original, doc, err := decodeSettingsDocument(path, raw)
	if err != nil {
		return "", err
	}
	if err := applySettingEdits(doc, edits); err != nil {
		return "", err
	}
	return persistSettingsDocument(home, path, doc, original)
}

func applySettingEdits(doc map[string]any, edits []SettingEdit) error {
	for _, edit := range edits {
		parts, err := splitSettingKey(edit.Key)
		if err != nil {
			return err
		}
		setNested(doc, parts, edit.Value)
	}
	return nil
}

func persistSettingsDocument(home, path string, doc map[string]any, original *fileDoc) (string, error) {
	output, err := encodeSettingsDocument(doc, original)
	if err != nil {
		return "", err
	}
	if err := persistSettings(home, path, output); err != nil {
		return "", err
	}
	return digestSettings(output), nil
}

func digestSettings(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func TrustedWorkspaces(home string) ([]string, error) {
	stored := readLayer(filepath.Join(home, "settings.toml"))
	if stored.err != nil {
		return nil, stored.err
	}
	if stored.doc == nil {
		return nil, nil
	}
	return slices.Clone(stored.doc.MCPTrustedWorkspaces), nil
}

func CanonicalWorkspace(cwd string) string {
	abs, err := filepath.Abs(cwd)
	if err != nil {
		return filepath.Clean(cwd)
	}
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return filepath.Clean(abs)
	}
	return filepath.Clean(resolved)
}

func TrustWorkspace(home, cwd string) error {
	clean := CanonicalWorkspace(cwd)
	trusted, err := TrustedWorkspaces(home)
	if err != nil {
		return err
	}
	if slices.Contains(trusted, clean) {
		return nil
	}
	return WriteSettings(home, SettingEdit{Key: "mcp_trusted_workspaces", Value: append(trusted, clean)})
}

func splitSettingKey(key string) ([]string, error) {
	parts := strings.Split(key, ".")
	if slices.Contains(parts, "") {
		return nil, errors.New("settings key must contain nonempty dot-separated parts")
	}
	return parts, nil
}

func readSettingsFile(path string) ([]byte, error) {
	raw, readErr := os.ReadFile(path)
	if readErr != nil && !errors.Is(readErr, os.ErrNotExist) {
		return nil, fmt.Errorf("read settings.toml: %w", readErr)
	}
	return raw, nil
}

func encodeSettingsDocument(doc map[string]any, original *fileDoc) ([]byte, error) {
	preserveEditStyleOrder(doc, original)
	var output bytes.Buffer
	if encodeErr := toml.NewEncoder(&output).EnableMarshalerInterface().Encode(doc); encodeErr != nil {
		return nil, fmt.Errorf("encode settings.toml: %w", encodeErr)
	}
	return output.Bytes(), nil
}

func persistSettings(home, path string, output []byte) error {
	if validateErr := validateSettings(home, path, output); validateErr != nil {
		return validateErr
	}
	if dirErr := auth.ProtectHomeDir(home); dirErr != nil {
		return fmt.Errorf("create config home: %w", dirErr)
	}
	if writeErr := writeSettings(path, output); writeErr != nil {
		return writeErr
	}
	return nil
}

func decodeSettingsDocument(path string, raw []byte) (*fileDoc, map[string]any, error) {
	original := layer{path: path}
	if len(raw) > 0 {
		original = decodeLayer(path, raw)
	}
	if original.err != nil {
		return nil, nil, original.err
	}
	doc := map[string]any{}
	if len(raw) > 0 {
		if err := toml.Unmarshal(raw, &doc); err != nil {
			return nil, nil, fmt.Errorf("settings.toml: %w", err)
		}
	}
	return original.doc, doc, nil
}

func preserveEditStyleOrder(doc map[string]any, original *fileDoc) {
	styles, ok := doc["edit_style"].(map[string]any)
	if !ok {
		return
	}
	var order []string
	if original != nil && original.EditStyle.Set {
		order = append(order, "default")
		for _, row := range original.EditStyle.Spec.Rows {
			order = append(order, row.Glob)
		}
	}
	for _, key := range slices.Sorted(maps.Keys(styles)) {
		if !slices.Contains(order, key) {
			order = append(order, key)
		}
	}
	doc["edit_style"] = orderedEditStyle{values: styles, order: order}
}

type orderedEditStyle struct {
	values map[string]any
	order  []string
}

func (s orderedEditStyle) MarshalTOML() ([]byte, error) {
	var output bytes.Buffer
	for _, key := range s.order {
		value, ok := s.values[key]
		if !ok {
			continue
		}
		encoded, err := toml.Marshal(map[string]any{key: value})
		if err != nil {
			return nil, fmt.Errorf("encode edit_style.%s: %w", key, err)
		}
		output.Write(encoded)
	}
	return output.Bytes(), nil
}

func validateSettings(home, path string, raw []byte) error {
	parsed := decodeLayer(path, raw)
	if parsed.err != nil {
		return parsed.err
	}
	cfg := defaultConfig(ProductDalgona, "")
	cfg.Home = home
	if err := cfg.merge(parsed); err != nil {
		return fmt.Errorf("validate settings.toml: %w", err)
	}
	if err := cfg.validate(); err != nil {
		return fmt.Errorf("validate settings.toml: %w", err)
	}
	return nil
}

func writeSettings(path string, raw []byte) error {
	file, err := os.CreateTemp(filepath.Dir(path), ".settings-*.tmp")
	if err != nil {
		return fmt.Errorf("create settings temporary file: %w", err)
	}
	temporary := file.Name()
	defer func() { _ = os.Remove(temporary) }()
	if _, err := file.Write(raw); err != nil {
		return errors.Join(fmt.Errorf("write settings.toml: %w", err), file.Close())
	}
	if err := file.Chmod(0o600); err != nil {
		return errors.Join(fmt.Errorf("chmod settings.toml: %w", err), file.Close())
	}
	if err := file.Sync(); err != nil {
		return errors.Join(fmt.Errorf("sync settings.toml: %w", err), file.Close())
	}
	if err := file.Close(); err != nil {
		return fmt.Errorf("close settings.toml: %w", err)
	}
	if err := os.Rename(temporary, path); err != nil {
		return fmt.Errorf("replace settings.toml: %w", err)
	}
	return syncDir(filepath.Dir(path))
}

func syncDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	directory, err := os.Open(dir)
	if err != nil {
		return fmt.Errorf("open %s: %w", dir, err)
	}
	syncErr := directory.Sync()
	closeErr := directory.Close()
	if syncErr != nil {
		return fmt.Errorf("fsync %s: %w", dir, syncErr)
	}
	return closeErr
}

func setNested(values map[string]any, parts []string, value any) {
	for _, part := range parts[:len(parts)-1] {
		nested, ok := values[part].(map[string]any)
		if !ok {
			nested = map[string]any{}
			values[part] = nested
		}
		values = nested
	}
	values[parts[len(parts)-1]] = value
}

func (c *Config) SettingsPath() string { return filepath.Join(c.Home, "settings.toml") }
