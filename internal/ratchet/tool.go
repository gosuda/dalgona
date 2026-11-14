package ratchet

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"path/filepath"
	"slices"
	"sync"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool"
)

const (
	Name = "ratchet"

	maxSummaryRunes = 200
	maxSeed         = 1<<53 - 1
	defaultFraction = 0.4
)

const schemaJSON = `{"type":"object","properties":{"action":{"type":"string","enum":["init","plan","split","approve","check","gate","train","status"]},"flow":{"type":"string"},"cases":{"type":"array","items":{"type":"string"}},"harness":{"type":"array","items":{"type":"string"}},"change":{"type":"array","items":{"type":"string"}},"off_limits":{"type":"array","items":{"type":"string"}},"target":{"type":"string"},"direction":{"type":"string","enum":["higher","lower"]},"hold":{"type":"array","items":{"type":"string"}},"directions":{"type":"object","additionalProperties":{"type":"string","enum":["higher","lower"]}},"reps":{"type":"integer"},"plateau":{"type":"integer"},"rounds":{"type":"integer"},"command":{"type":"string"},"strata":{"type":"object","additionalProperties":{"type":"string"}},"test_fraction":{"type":"number"},"seed":{"type":"integer"},"stage":{"type":"string","enum":["inputs","grader","plan"]},"question":{"type":"string"},"preview":{"type":"string"},"variant":{"type":"string"},"summary":{"type":"string"}},"required":["action","flow"],"additionalProperties":false}`

const description = "Run a ratchet flow: a held-out eval that keeps a change only when it wins on both train and test cases. State lives in .dalgona/ratchet/<flow>/. Actions: init sets case, harness, change and off-limits paths; plan sets the target, guardrails, reps, stop rule and runner command; split assigns case ids to train and test by stratum tag; approve asks the user to approve inputs, grader or plan and binds a content hash; check preflights a variant and returns its runner command; gate judges a finished run as keep, revert or rerun; train returns train rows, train traces and score history for the analyzer; status returns approvals and the round table. The result is JSON text."

var allowedMembers = map[string][]string{
	"init":    {"cases", "harness", "change", "off_limits"},
	"plan":    {"target", "direction", "hold", "directions", "reps", "plateau", "rounds", "command"},
	"split":   {"strata", "test_fraction", "seed"},
	"approve": {"stage", "question", "preview"},
	"check":   {"variant"},
	"gate":    {"variant", "summary"},
	"train":   {"variant"},
	"status":  nil,
}

var locks sync.Map

type Option struct {
	Label       string
	Description string
	Preview     string
}

type Question struct {
	Header  string
	Text    string
	Options []Option
}

type Answer struct {
	Label string
	Other string
}

var (
	ErrNoClient  = errors.New("ratchet approvals need an interactive client")
	ErrCancelled = errors.New("the question was cancelled")
)

type Asker interface {
	Ask(ctx context.Context, callID string, q Question) (Answer, error)
}

type Tool struct {
	asker Asker
}

func New(asker Asker) Tool {
	return Tool{asker: asker}
}

func (Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        Name,
		Description: description,
		Schema:      jsontext.Value(schemaJSON),
		Keep:        tool.KeepHead,
	}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

type args struct {
	Action string `json:"action"`
	Flow   string `json:"flow"`
	initArgs
	planArgs
	Strata       map[string]string `json:"strata"`
	TestFraction *float64          `json:"test_fraction"`
	Seed         *uint64           `json:"seed"`
	Stage        string            `json:"stage"`
	Question     string            `json:"question"`
	Preview      string            `json:"preview"`
	Variant      string            `json:"variant"`
	Summary      string            `json:"summary"`
}

type statusResult struct {
	Approvals  map[string]string `json:"approvals"`
	Best       *bestRound        `json:"best"`
	Done       bool              `json:"done"`
	DoneReason string            `json:"done_reason"`
	FlatRounds int               `json:"flat_rounds"`
	Flow       string            `json:"flow"`
	Plateau    bool              `json:"plateau"`
	Table      string            `json:"table"`
}

func (t Tool) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	if !tool.IsLocalRunner(env.Runner) {
		return tool.Result{}, errors.New("ratchet needs the local runner")
	}
	a, err := parseArgs(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	f, err := openFlow(env.Cwd, a.Flow)
	if err != nil {
		return tool.Result{}, err
	}
	res, err := t.dispatch(ctx, call.ID, f, a, flowLock(filepath.Join(env.Cwd, f.dir)))
	if cerr := f.close(); cerr != nil && err == nil {
		err = cerr
	}
	if err != nil {
		return tool.Result{}, fmt.Errorf("ratchet %s: %w", a.Action, err)
	}
	return res, nil
}

func (t Tool) dispatch(ctx context.Context, callID string, f *flow, a args, mu *sync.Mutex) (tool.Result, error) {
	if a.Action == "approve" {
		return t.approve(ctx, callID, f, a, mu)
	}
	mu.Lock()
	defer mu.Unlock()
	return act(callID, f, a)
}

func act(callID string, f *flow, a args) (tool.Result, error) {
	switch a.Action {
	case "init":
		return initFlow(f, a)
	case "plan":
		return planFlow(f, a)
	case "split":
		return splitFlow(f, callID, a)
	case "check":
		return checkFlow(f, a)
	case "gate":
		return gateFlow(f, a)
	case "train":
		return trainFlow(f, a)
	case "status":
		return statusFlow(f)
	}
	return tool.Result{}, fmt.Errorf("unknown action %q", a.Action)
}

func parseArgs(raw jsontext.Value) (args, error) {
	var members map[string]jsontext.Value
	if err := json.Unmarshal(raw, &members); err != nil {
		return args{}, fmt.Errorf("arguments: %w", err)
	}
	var a args
	if err := json.Unmarshal(raw, &a, json.RejectUnknownMembers(true)); err != nil {
		return args{}, fmt.Errorf("arguments: %w", err)
	}
	if err := checkMembers(a.Action, members); err != nil {
		return args{}, err
	}
	return a, nil
}

func checkMembers(action string, members map[string]jsontext.Value) error {
	allowed, ok := allowedMembers[action]
	if !ok {
		return fmt.Errorf("unknown action %q", action)
	}
	for _, key := range slices.Sorted(maps.Keys(members)) {
		if key == "action" || key == "flow" || slices.Contains(allowed, key) {
			continue
		}
		return fmt.Errorf("%s does not take %s", action, key)
	}
	return nil
}

func flowLock(dir string) *sync.Mutex {
	mu, _ := locks.LoadOrStore(dir, new(sync.Mutex))
	return mu.(*sync.Mutex)
}

func toolResult(v any) (tool.Result, error) {
	out, err := json.Marshal(v, json.Deterministic(true))
	if err != nil {
		return tool.Result{}, fmt.Errorf("encode result: %w", err)
	}
	return tool.Result{Output: out, OK: true}, nil
}

func initFlow(f *flow, a args) (tool.Result, error) {
	if _, err := f.load(); !errors.Is(err, errNoFlow) {
		if err != nil {
			return tool.Result{}, err
		}
		return tool.Result{}, fmt.Errorf("flow %q already exists", f.name)
	}
	s := &state{}
	if err := f.init(s, a.initArgs); err != nil {
		return tool.Result{}, err
	}
	if err := f.save(s); err != nil {
		return tool.Result{}, err
	}
	return toolResult(map[string]any{
		"cases":      s.CasesPaths,
		"change":     s.ChangePaths,
		"dir":        filepath.ToSlash(f.dir),
		"flow":       f.name,
		"harness":    s.HarnessPaths,
		"off_limits": s.OffLimits,
	})
}

func planFlow(f *flow, a args) (tool.Result, error) {
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	if err := s.applyPlan(a.planArgs); err != nil {
		return tool.Result{}, err
	}
	if err := f.save(s); err != nil {
		return tool.Result{}, err
	}
	return toolResult(map[string]any{"command": s.Command, "goal": s.Goal, "reps": s.Reps, "stop": s.Stop})
}

func splitFlow(f *flow, callID string, a args) (tool.Result, error) {
	if a.Seed != nil && *a.Seed > maxSeed {
		return tool.Result{}, fmt.Errorf("seed must be from 0 to %d", maxSeed)
	}
	fraction := defaultFraction
	if a.TestFraction != nil {
		fraction = *a.TestFraction
	}
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	if err := s.split(a.Strata, fraction, splitSeed(a, callID)); err != nil {
		return tool.Result{}, err
	}
	if err := f.save(s); err != nil {
		return tool.Result{}, err
	}
	return toolResult(map[string]any{
		"seed":   s.SplitSeed,
		"strata": strataCounts(s, a.Strata),
		"test":   len(s.TestIDs),
		"train":  len(s.TrainIDs),
	})
}

func splitSeed(a args, callID string) uint64 {
	if a.Seed != nil {
		return *a.Seed
	}
	sum := sha256.Sum256([]byte(callID))
	return binary.BigEndian.Uint64(sum[:8]) >> 11
}

func strataCounts(s *state, strata map[string]string) map[string]map[string]int {
	train := idSet(s.TrainIDs)
	counts := make(map[string]map[string]int)
	for id, tag := range strata {
		entry, ok := counts[tag]
		if !ok {
			entry = map[string]int{"test": 0, "train": 0}
			counts[tag] = entry
		}
		side := "test"
		if train[id] {
			side = "train"
		}
		entry[side]++
	}
	return counts
}

func checkFlow(f *flow, a args) (tool.Result, error) {
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	res, err := f.check(s, a.Variant)
	if err != nil {
		return tool.Result{}, err
	}
	return toolResult(res)
}

func gateFlow(f *flow, a args) (tool.Result, error) {
	if !variantName.MatchString(a.Variant) {
		return tool.Result{}, fmt.Errorf("variant must match %s: %s", variantPattern, a.Variant)
	}
	if a.Variant != "baseline" && a.Summary == "" {
		return tool.Result{}, errors.New("gate needs a summary of the change")
	}
	if utf8.RuneCountInString(a.Summary) > maxSummaryRunes {
		return tool.Result{}, fmt.Errorf("summary must be at most %d characters", maxSummaryRunes)
	}
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	res, err := f.gate(s, a.Variant, a.Summary)
	if err != nil {
		return tool.Result{}, err
	}
	return toolResult(res)
}

func trainFlow(f *flow, a args) (tool.Result, error) {
	if !variantName.MatchString(a.Variant) {
		return tool.Result{}, fmt.Errorf("variant must match %s: %s", variantPattern, a.Variant)
	}
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	view, err := f.train(s, a.Variant)
	if err != nil {
		return tool.Result{}, err
	}
	return toolResult(view)
}

func statusFlow(f *flow) (tool.Result, error) {
	s, err := f.load()
	if err != nil {
		return tool.Result{}, err
	}
	approvals, err := f.approvals(s)
	if err != nil {
		return tool.Result{}, err
	}
	done, reason := s.done()
	return toolResult(statusResult{
		Approvals:  approvals,
		Best:       s.Best,
		Done:       done,
		DoneReason: reason,
		FlatRounds: s.FlatRounds,
		Flow:       f.name,
		Plateau:    s.plateauReached(),
		Table:      renderTable(s),
	})
}
