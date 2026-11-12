package config

import (
	"errors"
	"fmt"
	"slices"
	"strings"
)

type SettingSpec struct {
	Key     string
	Default any
	Options []any
}

type SettingEdit struct {
	Key   string
	Value any
}

var statusItems = []string{"effort", "approval", "sandbox", "folder", "queue", "context", "mark"}

var InteractiveSettings = []SettingSpec{
	{Key: "approval", Default: "auto", Options: anys("auto", "yolo", "ask")},
	{Key: "verbosity", Default: "medium", Options: anys("low", "medium", "high")},
	{Key: "theme", Default: "auto", Options: anys("auto", "auto-soft", "night", "soft-night", "day", "soft-day")},
	{Key: "icons", Default: "auto", Options: anys("auto", "unicode", "nerd")},
	{Key: "status_hide", Default: []string{}, Options: anys(statusItems...)},
	{Key: "edit_review.enabled", Default: false, Options: anys(true, false)},
	{Key: "notifications", Default: true, Options: anys(true, false)},
}

func anys[T any](values ...T) []any {
	out := make([]any, 0, len(values))
	for _, value := range values {
		out = append(out, value)
	}
	return out
}

func optionsOf(key string) []string {
	for _, spec := range InteractiveSettings {
		if spec.Key != key {
			continue
		}
		out := make([]string, 0, len(spec.Options))
		for _, option := range spec.Options {
			out = append(out, fmt.Sprint(option))
		}
		return out
	}
	return nil
}

func validateStatusHide(ids []string) error {
	for _, id := range ids {
		if id == "model" {
			return errors.New("status_hide: model cannot be hidden")
		}
		if !slices.Contains(statusItems, id) {
			return fmt.Errorf("status_hide: unsupported item %s", id)
		}
	}
	return nil
}

type settingSetter func(c *Config, value any) error

var settingSetters = map[string]settingSetter{
	"model":               textSetting("model", nil, func(c *Config, v string) { c.Model = v }),
	"reasoning":           textSetting("reasoning", []string{"default", "low", "medium", "high", "xhigh", "max"}, func(c *Config, v string) { c.Reasoning = Reasoning(v) }),
	"approval":            textSetting("approval", optionsOf("approval"), func(c *Config, v string) { c.Approval = Approval(v) }),
	"sandbox":             textSetting("sandbox", []string{"workspace-write", "off"}, func(c *Config, v string) { c.Sandbox = SandboxMode(v) }),
	"verbosity":           textSetting("verbosity", optionsOf("verbosity"), func(c *Config, v string) { c.Verbosity = Verbosity(v) }),
	"theme":               textSetting("theme", optionsOf("theme"), func(c *Config, v string) { c.Theme = Theme(v) }),
	"icons":               textSetting("icons", optionsOf("icons"), func(c *Config, v string) { c.Icons = Icons(v) }),
	"status_hide":         statusHideSetting,
	"edit_review.enabled": flagSetting("edit_review.enabled", func(c *Config, v bool) { c.EditReview.Enabled = v }),
	"notifications":       flagSetting("notifications", func(c *Config, v bool) { c.Notifications = v }),
}

var statusHideSetting settingSetter = func(c *Config, value any) error {
	ids, ok := value.([]string)
	if !ok {
		return errors.New("status_hide must be a list of strings")
	}
	if err := validateStatusHide(ids); err != nil {
		return err
	}
	c.StatusHide = slices.Clone(ids)
	return nil
}

func textSetting(key string, options []string, set func(*Config, string)) settingSetter {
	return func(c *Config, value any) error {
		text, ok := value.(string)
		if !ok {
			return fmt.Errorf("%s must be a string", key)
		}
		if options != nil && !slices.Contains(options, text) {
			return fmt.Errorf("%s must be one of %s", key, strings.Join(options, ", "))
		}
		set(c, text)
		return nil
	}
}

func flagSetting(key string, set func(*Config, bool)) settingSetter {
	return func(c *Config, value any) error {
		flag, ok := value.(bool)
		if !ok {
			return fmt.Errorf("%s must be a boolean", key)
		}
		set(c, flag)
		return nil
	}
}

func (c Config) With(edits ...SettingEdit) (Config, error) {
	next := c
	for _, edit := range edits {
		setter, ok := settingSetters[edit.Key]
		if !ok {
			return Config{}, fmt.Errorf("unsupported setting %s", edit.Key)
		}
		if err := setter(&next, edit.Value); err != nil {
			return Config{}, err
		}
	}
	return next, nil
}

var settingGetters = map[string]func(Config) any{
	"model":               func(c Config) any { return c.Model },
	"reasoning":           func(c Config) any { return string(c.Reasoning) },
	"approval":            func(c Config) any { return string(c.Approval) },
	"sandbox":             func(c Config) any { return string(c.Sandbox) },
	"verbosity":           func(c Config) any { return string(c.Verbosity) },
	"theme":               func(c Config) any { return string(c.Theme) },
	"icons":               func(c Config) any { return string(c.Icons) },
	"status_hide":         func(c Config) any { return slices.Clone(c.StatusHide) },
	"edit_review.enabled": func(c Config) any { return c.EditReview.Enabled },
	"notifications":       func(c Config) any { return c.Notifications },
}

func (c Config) Setting(key string) any {
	get, ok := settingGetters[key]
	if !ok {
		return nil
	}
	return get(c)
}
