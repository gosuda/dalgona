package config

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/pelletier/go-toml/v2"
)

type Product string

const (
	ProductDal     Product = "dal"
	ProductDalgona Product = "dalgona"
)

func (p Product) Home() string {
	key, dir := "DALGONA_HOME", ".dalgona"
	if p == ProductDal {
		key, dir = "DAL_HOME", ".dal"
	}
	if value := os.Getenv(key); value != "" {
		return value
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return dir
	}
	return filepath.Join(home, dir)
}

func AnchorHome() error {
	for _, key := range []string{"DALGONA_HOME", "DAL_HOME"} {
		value := os.Getenv(key)
		if value == "" || filepath.IsAbs(value) {
			continue
		}
		wd, err := os.Getwd()
		if err != nil {
			return fmt.Errorf("config: resolve home %s: %w", key, err)
		}
		if err := os.Setenv(key, filepath.Join(wd, value)); err != nil {
			return fmt.Errorf("config: anchor home %s: %w", key, err)
		}
	}
	return nil
}

func productHome(product Product) (string, error) {
	home := product.Home()
	if !filepath.IsAbs(home) {
		return "", fmt.Errorf("config: home %q is not absolute; resolve it at process start", home)
	}
	return home, nil
}

func newConfig(product Product, cwd string) (Config, error) {
	home, err := productHome(product)
	if err != nil {
		return Config{}, err
	}
	cfg := defaultConfigWithHome(product, home, cwd)
	if cfg.Cwd == "" {
		cfg.Cwd = "."
		if directory, err := os.Getwd(); err == nil {
			cfg.Cwd = directory
		}
	}
	return cfg, nil
}

type (
	Flags struct {
		Model, Reasoning, Approval, ToolMode, Sandbox, Runner, Remote, Cwd string
		Advisor                                                            bool
	}
	Reasoning string
)

const (
	ReasoningDefault Reasoning = "default"
	ReasoningLow     Reasoning = "low"
	ReasoningMedium  Reasoning = "medium"
	ReasoningHigh    Reasoning = "high"
	ReasoningXHigh   Reasoning = "xhigh"
	ReasoningMax     Reasoning = "max"
)

type Approval string

const (
	ApprovalAuto Approval = "auto"
	ApprovalYolo Approval = "yolo"
	ApprovalAsk  Approval = "ask"
)

type SandboxMode string

const (
	SandboxWorkspaceWrite SandboxMode = "workspace-write"
	SandboxOff            SandboxMode = "off"
)

type ToolMode string

const (
	ToolModeNormal    ToolMode = "normal"
	ToolModeEvalFirst ToolMode = "eval-first"
	ToolModeEvalOnly  ToolMode = "eval-only"
)

type (
	EditStyleRow  struct{ Glob, Style string }
	EditStyleSpec struct {
		Rows    []EditStyleRow
		Default string
	}
)

func (s EditStyleSpec) EditStyle(model string) string {
	for _, row := range s.Rows {
		if globMatch(row.Glob, strings.ToLower(model)) {
			return row.Style
		}
	}
	return s.Default
}

func (s EditStyleSpec) ForModel(model string) string { return s.EditStyle(model) }

func globMatch(pattern, value string) bool {
	patternRunes := []rune(strings.ToLower(pattern))
	valueRunes := []rune(value)
	patternIndex, valueIndex := 0, 0
	starIndex, starValueIndex := -1, 0
	for valueIndex < len(valueRunes) {
		if globCharacterMatches(patternRunes, patternIndex, valueRunes, valueIndex) {
			patternIndex++
			valueIndex++
			continue
		}
		if globStarAt(patternRunes, patternIndex) {
			starIndex = patternIndex
			starValueIndex = valueIndex
			patternIndex++
			continue
		}
		if starIndex < 0 {
			return false
		}
		patternIndex = starIndex + 1
		starValueIndex++
		valueIndex = starValueIndex
	}
	return onlyGlobStars(patternRunes[patternIndex:])
}

func globCharacterMatches(pattern []rune, patternIndex int, value []rune, valueIndex int) bool {
	return patternIndex < len(pattern) && (pattern[patternIndex] == '?' || pattern[patternIndex] == value[valueIndex])
}

func globStarAt(pattern []rune, index int) bool {
	return index < len(pattern) && pattern[index] == '*'
}

func onlyGlobStars(pattern []rune) bool {
	for _, char := range pattern {
		if char != '*' {
			return false
		}
	}
	return true
}

type (
	KeywordSpec struct {
		Notice, Text string
		Hue          [2]int
		Effort       string
		Requires     []string
		Enabled      bool
	}
	RunnerSpec    struct{ Service, Image, Template, URL string }
	RemoteSpec    struct{ Service, Image, Template, URL, BoxID string }
	ProviderSpec  struct{ BaseURL, APIKeyEnv string }
	ModelSpec     struct{ ContextWindow int }
	MCPServerSpec struct {
		Command string
		Args    []string
		Env     map[string]string
		URL     string
	}
	DiscoveryConfig       struct{ Sources []string }
	ApprovalDeciderConfig struct {
		Decider   string
		Threshold float64
	}
	EvalConfig struct {
		Uses []string
		Set  bool
	}
	SecretsConfig struct{ Patterns []string }
	HerdrConfig   struct{ SubagentPanes bool }
	WebConfig     struct{ Listen, Token, CertFile, KeyFile string }
	RolesConfig   struct{ Smol, Slow, Advisor string }
	origin        struct {
		file string
		line int
	}
)

type Config struct {
	Home                                  string
	Product                               Product
	Cwd                                   string
	Model                                 string
	Reasoning                             Reasoning
	Approval                              Approval
	ToolMode                              ToolMode
	Sandbox                               SandboxMode
	SandboxWritableRoots                  []string
	EditStyle                             EditStyleSpec
	CompactionThreshold                   float64
	CompactionKeepTokens                  int
	StreamStartTimeout, StreamIdleTimeout time.Duration
	Runner, Remote                        string
	Providers                             map[string]ProviderSpec
	Models                                map[string]ModelSpec
	Runners                               map[string]RunnerSpec
	Remotes                               map[string]RemoteSpec
	MCPServers                            map[string]MCPServerSpec
	Discovery                             DiscoveryConfig
	ApprovalDecider                       ApprovalDeciderConfig
	Eval                                  EvalConfig
	Secrets                               SecretsConfig
	Keywords                              map[string]KeywordSpec
	Herdr                                 HerdrConfig
	Web                                   WebConfig
	Roles                                 RolesConfig
	Advisor                               AdvisorConfig
	Advisors                              map[string]AdvisorSpec
	advisorEntries                        map[string]advisorsDoc
	lines                                 map[string]origin
}

func defaultConfig(product Product, cwd string) Config {
	return defaultConfigWithHome(product, product.Home(), cwd)
}

func defaultConfigWithHome(product Product, home, cwd string) Config {
	return Config{
		Home: home, Product: product, Cwd: cwd,
		Reasoning: ReasoningDefault, Approval: ApprovalAuto, ToolMode: ToolModeNormal,
		Sandbox: SandboxWorkspaceWrite, Runner: "local",
		EditStyle: EditStyleSpec{Default: "anchor"}, CompactionThreshold: .8,
		CompactionKeepTokens: 20000, StreamStartTimeout: time.Minute,
		StreamIdleTimeout: 5 * time.Minute, ApprovalDecider: ApprovalDeciderConfig{Threshold: .9},
		Web: WebConfig{Listen: "127.0.0.1:7437"}, Providers: map[string]ProviderSpec{},
		Models: map[string]ModelSpec{}, Runners: map[string]RunnerSpec{}, Remotes: map[string]RemoteSpec{},
		MCPServers: map[string]MCPServerSpec{}, Keywords: map[string]KeywordSpec{},
		lines: map[string]origin{}, Advisor: defaultAdvisor(), advisorEntries: map[string]advisorsDoc{},
	}
}

func Load(product Product, cwd string, flags Flags) (Config, error) {
	cfg, err := newConfig(product, cwd)
	if err != nil {
		return Config{}, err
	}
	for _, name := range []string{"config.toml", "settings.toml"} {
		layer := readLayer(filepath.Join(cfg.Home, name))
		if layer.err != nil {
			return Config{}, layer.err
		}
		if layer.doc == nil {
			continue
		}
		if err := validateProductLayer(product, layer); err != nil {
			return Config{}, err
		}
		if err := validateTrustedWorkspaces(layer); err != nil {
			return Config{}, err
		}
		if err := cfg.merge(layer); err != nil {
			return Config{}, err
		}
	}
	cfg.applyFlags(flags)
	if err := cfg.validate(); err != nil {
		return Config{}, err
	}
	cfg.resolveAdvisors()
	return cfg, nil
}

func validateTrustedWorkspaces(l layer) error {
	for _, workspace := range l.doc.MCPTrustedWorkspaces {
		if workspace == "" {
			return fmt.Errorf("%s: mcp_trusted_workspaces entry must be a nonempty path", filepath.Base(l.path))
		}
	}
	return nil
}

type layer struct {
	path string
	doc  *fileDoc
	err  error
	raw  []byte
}

func readLayer(path string) layer {
	raw, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return layer{path: path}
	}
	if err != nil {
		return layer{path: path, err: fmt.Errorf("config: %s: %w", path, err)}
	}
	return decodeLayer(path, raw)
}

func decodeLayer(path string, raw []byte) layer {
	var doc fileDoc
	decoder := toml.NewDecoder(bytes.NewReader(raw)).DisallowUnknownFields().EnableUnmarshalerInterface()
	if err := decoder.Decode(&doc); err != nil {
		return layer{path: path, err: formatDecode(path, err)}
	}
	return layer{path: path, doc: &doc, raw: raw}
}

func formatDecode(path string, err error) error {
	strict, ok := errors.AsType[*toml.StrictMissingError](err)
	if ok && len(strict.Errors) > 0 {
		decodeError := &strict.Errors[0]
		line, _ := decodeError.Position()
		return fmt.Errorf("%s:%d: unknown key %q", filepath.Base(path), line, strings.Join(decodeError.Key(), "."))
	}
	return fmt.Errorf("%s: %w", filepath.Base(path), err)
}
