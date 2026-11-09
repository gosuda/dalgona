package config

import (
	"cmp"
	"maps"
	"regexp"
	"slices"
	"strings"
)

type (
	AdvisorReview string
	AdvisorSync   string
)

const (
	AdvisorReviewTurn     AdvisorReview = "turn"
	AdvisorReviewAgentEnd AdvisorReview = "agent-end"

	AdvisorSyncOff    AdvisorSync = "off"
	AdvisorSyncFinal  AdvisorSync = "final"
	AdvisorSyncStrict AdvisorSync = "strict"

	DefaultAdvisorName = "advisor"
)

type AdvisorConfig struct {
	Enabled          bool
	Review           AdvisorReview
	Interval         int
	Sync             AdvisorSync
	ContinueSeverity string
	ImmuneTurns      int
	MaxNotes         int
	Tools            []string
	Subagents        bool
}

type AdvisorSpec struct {
	Model, Instructions string
	Review              AdvisorReview
	Interval            int
	Sync                AdvisorSync
	Tools               []string
	Enabled             bool
}

type (
	advisorDoc struct {
		Enabled          *bool    `toml:"enabled"`
		Review           *string  `toml:"review"`
		Interval         *int     `toml:"interval"`
		Sync             *string  `toml:"sync"`
		ContinueSeverity *string  `toml:"continue_severity"`
		ImmuneTurns      *int     `toml:"immune_turns"`
		MaxNotes         *int     `toml:"max_notes"`
		Tools            []string `toml:"tools"`
		Subagents        *bool    `toml:"subagents"`
	}
	advisorsDoc struct {
		Model        *string  `toml:"model"`
		Instructions *string  `toml:"instructions"`
		Review       *string  `toml:"review"`
		Interval     *int     `toml:"interval"`
		Sync         *string  `toml:"sync"`
		Tools        []string `toml:"tools"`
		Enabled      *bool    `toml:"enabled"`
	}
)

func defaultAdvisor() AdvisorConfig {
	return AdvisorConfig{
		Review: AdvisorReviewTurn, Interval: 1, Sync: AdvisorSyncOff, ContinueSeverity: "blocker",
		ImmuneTurns: 3, MaxNotes: 3, Tools: []string{"read", "search"},
	}
}

func (r RolesConfig) Configured(role string) string {
	switch role {
	case "smol":
		return r.Smol
	case "slow":
		return r.Slow
	case "advisor":
		return cmp.Or(r.Advisor, r.Slow)
	}
	return ""
}

func (c *Config) mergeAdvisor(doc *fileDoc) {
	source := doc.Advisor
	setBool(source.Enabled, &c.Advisor.Enabled)
	setString(source.Review, (*string)(&c.Advisor.Review))
	setInt(source.Interval, &c.Advisor.Interval)
	setString(source.Sync, (*string)(&c.Advisor.Sync))
	setString(source.ContinueSeverity, &c.Advisor.ContinueSeverity)
	setInt(source.ImmuneTurns, &c.Advisor.ImmuneTurns)
	setInt(source.MaxNotes, &c.Advisor.MaxNotes)
	if source.Tools != nil {
		c.Advisor.Tools = source.Tools
	}
	setBool(source.Subagents, &c.Advisor.Subagents)
	for name, entry := range doc.Advisors {
		c.advisorEntries[name] = mergeAdvisorEntry(c.advisorEntries[name], entry)
	}
}

func mergeAdvisorEntry(target, source advisorsDoc) advisorsDoc {
	target.Model = cmp.Or(source.Model, target.Model)
	target.Instructions = cmp.Or(source.Instructions, target.Instructions)
	target.Review = cmp.Or(source.Review, target.Review)
	target.Interval = cmp.Or(source.Interval, target.Interval)
	target.Sync = cmp.Or(source.Sync, target.Sync)
	target.Enabled = cmp.Or(source.Enabled, target.Enabled)
	if source.Tools != nil {
		target.Tools = source.Tools
	}
	return target
}

func setBool(source *bool, target *bool) {
	if source != nil {
		*target = *source
	}
}

func setInt(source *int, target *int) {
	if source != nil {
		*target = *source
	}
}

func (c *Config) resolveAdvisors() {
	entries := c.advisorEntries
	if len(entries) == 0 {
		entries = map[string]advisorsDoc{DefaultAdvisorName: {}}
	}
	c.Advisors = make(map[string]AdvisorSpec, len(entries))
	for name, entry := range entries {
		c.Advisors[name] = c.resolveAdvisor(entry)
	}
}

func (c *Config) resolveAdvisor(entry advisorsDoc) AdvisorSpec {
	spec := AdvisorSpec{
		Review: c.Advisor.Review, Interval: c.Advisor.Interval, Sync: c.Advisor.Sync,
		Tools: c.Advisor.Tools, Enabled: true,
	}
	setString(entry.Model, &spec.Model)
	setString(entry.Instructions, &spec.Instructions)
	setString(entry.Review, (*string)(&spec.Review))
	setInt(entry.Interval, &spec.Interval)
	setString(entry.Sync, (*string)(&spec.Sync))
	setBool(entry.Enabled, &spec.Enabled)
	if entry.Tools != nil {
		spec.Tools = entry.Tools
	}
	return spec
}

var advisorNameRe = regexp.MustCompile(`^[a-z][a-z0-9_-]{0,31}$`)

type advisorCheck struct {
	key, message string
	bad          bool
}

func (c *Config) validateAdvisors() []error {
	review, sync := string(c.Advisor.Review), string(c.Advisor.Sync)
	base := advisorsDoc{Review: &review, Interval: &c.Advisor.Interval, Sync: &sync}
	checks := advisorFieldChecks(base)
	checks = append(checks,
		advisorCheck{key: "continue_severity", bad: !one(c.Advisor.ContinueSeverity, "nit", "concern", "blocker"), message: "continue_severity must be one of nit, concern, blocker"},
		advisorCheck{key: "immune_turns", bad: c.Advisor.ImmuneTurns < 0, message: "immune_turns must not be negative"},
		advisorCheck{key: "max_notes", bad: c.Advisor.MaxNotes < 0, message: "max_notes must not be negative"},
	)
	errs := c.advisorErrors("advisor", checks)
	for _, name := range slices.Sorted(maps.Keys(c.advisorEntries)) {
		errs = append(errs, c.validateAdvisorEntry(name, c.advisorEntries[name])...)
	}
	return errs
}

func (c *Config) validateAdvisorEntry(name string, entry advisorsDoc) []error {
	table := "advisors." + name
	if !advisorNameRe.MatchString(name) {
		return []error{c.valueError(table, "["+table+"] name must be lowercase letters, digits, _ or -, 1-32 chars")}
	}
	checks := advisorFieldChecks(entry)
	checks = append(checks, advisorCheck{key: "model", bad: entry.Model != nil && !validAdvisorModel(*entry.Model), message: "model must be smol, default, slow, advisor, or provider/model"})
	return c.advisorErrors(table, checks)
}

func advisorFieldChecks(fields advisorsDoc) []advisorCheck {
	return []advisorCheck{
		{key: "review", bad: fields.Review != nil && !one(*fields.Review, "turn", "agent-end"), message: "review must be one of turn, agent-end"},
		{key: "interval", bad: fields.Interval != nil && *fields.Interval < 1, message: "interval must be at least 1"},
		{key: "sync", bad: fields.Sync != nil && !one(*fields.Sync, "off", "final", "strict"), message: "sync must be one of off, final, strict"},
	}
}

func (c *Config) advisorErrors(table string, checks []advisorCheck) []error {
	var errs []error
	for _, check := range checks {
		if check.bad {
			errs = append(errs, c.valueError(table+"."+check.key, "["+table+"] "+check.message))
		}
	}
	return errs
}

func validAdvisorModel(model string) bool {
	if model == "" || one(model, "smol", "default", "slow", "advisor") {
		return true
	}
	provider, id, ok := strings.Cut(model, "/")
	return ok && provider != "" && id != ""
}
