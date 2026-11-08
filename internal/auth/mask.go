package auth

import (
	"encoding/json/jsontext"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

type pattern struct {
	re    *regexp.Regexp
	index int
}

type replacement struct {
	from string
	to   string
}

func compilePatterns(patterns []string) ([]pattern, error) {
	compiled := make([]pattern, 0, len(patterns))
	for index, source := range patterns {
		re, err := regexp.Compile(source)
		if err != nil {
			return nil, fmt.Errorf("secrets pattern %d: %w", index, err)
		}
		compiled = append(compiled, pattern{re: re, index: index})
	}
	return compiled, nil
}

const maskPersistenceFailure = "secret masking failed: pattern state was not persisted"

func (s *Store) Mask(text string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	replacements := s.credentialReplacements()
	patternReplacements, changed := s.patternReplacements(text)
	replacements = append(replacements, patternReplacements...)
	s.secretsDirty = s.secretsDirty || changed
	if s.secretsDirty {
		if err := s.persistSecrets(); err != nil {
			s.maskErr = fmt.Errorf("persist pattern secrets: %w", err)
			return maskPersistenceFailure
		}
		s.secretsDirty = false
		s.maskErr = nil
	}
	return applyReplacements(text, replacements)
}

func (s *Store) Unmask(text string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	replacements := invertReplacements(s.credentialReplacements())
	for placeholder, secret := range s.secrets {
		replacements = append(replacements, replacement{from: placeholder, to: secret})
	}
	return applyReplacements(text, replacements)
}

func (s *Store) MaskError() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.maskErr
}

func invertReplacements(items []replacement) []replacement {
	inverted := make([]replacement, 0, len(items))
	for _, item := range items {
		inverted = append(inverted, replacement{from: item.to, to: item.from})
	}
	return inverted
}

func (s *Store) credentialReplacements() []replacement {
	items := make([]replacement, 0, len(s.data)+8)
	for _, provider := range sortedCredentialIDs(s.data) {
		items = append(items, providerReplacements(provider, s.data[provider])...)
	}
	for _, entry := range os.Environ() {
		name, value, ok := strings.Cut(entry, "=")
		if ok && secretEnvironmentName(name) {
			items = appendSecret(items, replacement{from: value, to: "{{secret:" + name + "}}"})
		}
	}
	return items
}

func providerReplacements(provider string, credential Credential) []replacement {
	var items []replacement
	base := "{{secret:auth:" + provider
	baseUsed := false
	if credential.Key != "" {
		items = appendSecret(items, replacement{from: credential.Key, to: base + "}}"})
		baseUsed = true
	}
	if credential.Access != "" {
		placeholder := base + "}}"
		if baseUsed {
			placeholder = base + ":access}}"
		}
		items = appendSecret(items, replacement{from: credential.Access, to: placeholder})
		baseUsed = true
	}
	if credential.Refresh != "" {
		placeholder := base + "}}"
		if baseUsed {
			placeholder = base + ":refresh}}"
		}
		items = appendSecret(items, replacement{from: credential.Refresh, to: placeholder})
	}
	return items
}

func appendSecret(items []replacement, item replacement) []replacement {
	if len(item.from) < 8 {
		return items
	}
	return append(items, item)
}

func secretEnvironmentName(name string) bool {
	return strings.HasSuffix(name, "_API_KEY") || strings.HasSuffix(name, "_TOKEN") || strings.HasSuffix(name, "_SECRET") || strings.HasSuffix(name, "_PASSWORD")
}

func (s *Store) patternReplacements(text string) ([]replacement, bool) {
	var items []replacement
	changed := false
	for _, configured := range s.patterns {
		for _, location := range configured.re.FindAllStringIndex(text, -1) {
			value := text[location[0]:location[1]]
			if len(value) < 8 {
				continue
			}
			placeholder, added := s.patternPlaceholder(configured.index, value)
			changed = changed || added
			items = append(items, replacement{from: value, to: placeholder})
		}
	}
	return items, changed
}

func (s *Store) patternPlaceholder(index int, value string) (string, bool) {
	prefix := "{{secret:pattern" + strconv.Itoa(index) + ":"
	for placeholder, secret := range s.secrets {
		if strings.HasPrefix(placeholder, prefix) && secret == value {
			return placeholder, false
		}
	}
	next := s.nextPatternNumber(prefix)
	placeholder := prefix + strconv.Itoa(next) + "}}"
	s.secrets[placeholder] = value
	return placeholder, true
}

func (s *Store) nextPatternNumber(prefix string) int {
	next := 0
	for placeholder := range s.secrets {
		number, hasPrefix := strings.CutPrefix(placeholder, prefix)
		if !hasPrefix {
			continue
		}
		number, hasSuffix := strings.CutSuffix(number, "}}")
		if !hasSuffix {
			continue
		}
		value, err := strconv.Atoi(number)
		if err == nil && value >= next {
			next = value + 1
		}
	}
	return next
}

func applyReplacements(text string, items []replacement) string {
	slices.SortFunc(items, func(a, b replacement) int {
		if len(a.from) != len(b.from) {
			return len(b.from) - len(a.from)
		}
		if a.from != b.from {
			return strings.Compare(a.from, b.from)
		}
		return strings.Compare(a.to, b.to)
	})
	var output strings.Builder
	position, written := 0, 0
	for position < len(text) {
		item, ok := replacementAt(text[position:], items)
		if !ok {
			position++
			continue
		}
		output.WriteString(text[written:position])
		output.WriteString(item.to)
		position += len(item.from)
		written = position
	}
	if written == 0 {
		return text
	}
	output.WriteString(text[written:])
	return output.String()
}

func replacementAt(text string, items []replacement) (replacement, bool) {
	for _, item := range items {
		if strings.HasPrefix(text, item.from) {
			return item, true
		}
	}
	return replacement{}, false
}

func (s *Store) persistSecrets() error {
	return writeSecure(filepath.Join(s.dir, "secrets.json"), func(encoder *jsontext.Encoder) error {
		if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
			return fmt.Errorf("write secrets object: %w", err)
		}
		for _, placeholder := range slices.Sorted(maps.Keys(s.secrets)) {
			if err := encoder.WriteToken(jsontext.String(placeholder)); err != nil {
				return fmt.Errorf("write secret placeholder: %w", err)
			}
			if err := encoder.WriteToken(jsontext.String(s.secrets[placeholder])); err != nil {
				return fmt.Errorf("write secret value: %w", err)
			}
		}
		if err := encoder.WriteToken(jsontext.EndObject); err != nil {
			return fmt.Errorf("finish secrets object: %w", err)
		}
		return nil
	})
}
