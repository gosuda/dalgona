package auth

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"log/slog"
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

const (
	maskPersistenceFailure = "secret masking failed: pattern state was not persisted"
	maskReloadFailure      = "secret masking failed: auth.json could not be reread"
)

func (s *Store) Mask(text string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.maskLocked(text)
}

func (s *Store) MaskWithError(text string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	masked := s.maskLocked(text)
	if s.maskErr != nil {
		return "", s.maskErr
	}
	return masked, nil
}

func (s *Store) maskLocked(text string) string {
	if err := s.reloadAuthIfChanged(); err != nil {
		s.maskErr = fmt.Errorf("reload credentials: %w", err)
		return maskReloadFailure
	}
	if !s.secretsDirty {
		s.maskErr = nil
	}
	replacements := s.credentialReplacements()
	hits := s.collectPatternHits(text)
	if len(hits) > 0 || s.secretsDirty {
		if err := s.syncPatternSecrets(hits); err != nil {
			s.secretsDirty = true
			s.maskErr = fmt.Errorf("persist pattern secrets: %w", err)
			return maskPersistenceFailure
		}
		s.secretsDirty = false
		s.maskErr = nil
	}
	replacements = append(replacements, s.lookupPatternReplacements(text)...)
	return applyReplacements(text, replacements)
}

func (s *Store) Unmask(text string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reloadForMask()
	s.refreshSecrets()
	replacements := invertReplacements(s.credentialReplacements())
	for placeholder, secret := range s.secrets {
		replacements = append(replacements, replacement{from: placeholder, to: secret})
	}
	return applyReplacements(text, replacements)
}

func (s *Store) UnmaskStrict(text string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reloadForMask()
	s.refreshSecrets()
	replacements := invertReplacements(s.credentialReplacements())
	for placeholder, secret := range s.secrets {
		replacements = append(replacements, replacement{from: placeholder, to: secret})
	}
	out := applyReplacements(text, replacements)
	if token, ok := unresolvedPlaceholder(out); ok {
		return "", fmt.Errorf("auth: secret placeholder %q cannot be restored: secrets.json is missing or home changed", token)
	}
	return out, nil
}

func unresolvedPlaceholder(text string) (string, bool) {
	for offset := 0; offset < len(text); {
		rel := strings.Index(text[offset:], "{{secret:")
		if rel < 0 {
			return "", false
		}
		start := offset + rel
		endRel := strings.Index(text[start:], "}}")
		if endRel < 0 {
			return "", false
		}
		token := text[start : start+endRel+2]
		if isSecretPlaceholder(token) {
			return token, true
		}
		offset = start + 1
	}
	return "", false
}

func isSecretPlaceholder(token string) bool {
	const prefix = "{{secret:"
	const suffix = "}}"
	if !strings.HasPrefix(token, prefix) || !strings.HasSuffix(token, suffix) {
		return false
	}
	inner := token[len(prefix) : len(token)-len(suffix)]
	if inner == "" {
		return false
	}
	if strings.HasPrefix(inner, "pattern") {
		return isPatternPlaceholder(inner)
	}
	if strings.HasPrefix(inner, "auth:") {
		return isAuthPlaceholder(inner)
	}
	return isEnvPlaceholder(inner)
}

func isPatternPlaceholder(inner string) bool {
	rest, ok := strings.CutPrefix(inner, "pattern")
	if !ok {
		return false
	}
	index, number, ok := strings.Cut(rest, ":")
	if !ok || index == "" || number == "" {
		return false
	}
	return isDecimalDigits(index) && isDecimalDigits(number)
}

func isAuthPlaceholder(inner string) bool {
	rest, ok := strings.CutPrefix(inner, "auth:")
	if !ok || rest == "" {
		return false
	}
	provider, kind, found := strings.Cut(rest, ":")
	if !isProviderID(provider) {
		return false
	}
	if !found {
		return true
	}
	if strings.Contains(kind, ":") {
		return false
	}
	return kind == "access" || kind == "refresh"
}

func isEnvPlaceholder(inner string) bool {
	if strings.ContainsAny(inner, ":{}") {
		return false
	}
	if !secretEnvironmentName(inner) {
		return false
	}
	return isEnvName(inner)
}

func isDecimalDigits(text string) bool {
	for i := range len(text) {
		if text[i] < '0' || text[i] > '9' {
			return false
		}
	}
	return len(text) > 0
}

func isEnvName(text string) bool {
	if text == "" {
		return false
	}
	for i := range len(text) {
		if !isEnvChar(text[i]) {
			return false
		}
	}
	return !isDigit(text[0])
}

func isEnvChar(c byte) bool {
	switch {
	case c == '_':
		return true
	case isDigit(c):
		return true
	case c >= 'A' && c <= 'Z':
		return true
	case c >= 'a' && c <= 'z':
		return true
	default:
		return false
	}
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

func isProviderID(text string) bool {
	if text == "" {
		return false
	}
	for i := range len(text) {
		c := text[i]
		if c <= ' ' || c == ':' || c == '{' || c == '}' {
			return false
		}
	}
	return true
}

func (s *Store) MaskError() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.maskErr
}

func (s *Store) reloadForMask() {
	if err := s.reloadAuthIfChanged(); err != nil {
		slog.Warn("auth: keeping cached credentials", "error", err)
	}
}

func (s *Store) refreshSecrets() {
	unlock, err := lockAuth(s.dir)
	if err != nil {
		slog.Warn("auth: keeping cached pattern secrets", "error", err)
		return
	}
	disk, readErr := readSecretsMap(s.dir)
	unlockErr := unlock()
	if readErr != nil {
		slog.Warn("auth: keeping cached pattern secrets", "error", readErr)
		return
	}
	if unlockErr != nil {
		slog.Warn("auth: releasing pattern secret lock", "error", unlockErr)
	}
	s.secrets = mergeSecretMaps(disk, s.secrets)
}

type patternHit struct {
	index int
	value string
}

func patternPrefix(index int) string {
	return "{{secret:pattern" + strconv.Itoa(index) + ":"
}

func placeholderPrefix(placeholder string) (string, bool) {
	const base = "{{secret:pattern"
	after, ok := strings.CutPrefix(placeholder, base)
	if !ok {
		return "", false
	}
	indexPart, remainder, ok := strings.Cut(after, ":")
	if !ok {
		return "", false
	}
	if _, err := strconv.Atoi(indexPart); err != nil {
		return "", false
	}
	number, ok := strings.CutSuffix(remainder, "}}")
	if !ok {
		return "", false
	}
	if _, err := strconv.Atoi(number); err != nil {
		return "", false
	}
	return base + indexPart + ":", true
}

func findPatternPlaceholder(secrets map[string]string, prefix, value string) (string, bool) {
	for placeholder, secret := range secrets {
		if strings.HasPrefix(placeholder, prefix) && secret == value {
			return placeholder, true
		}
	}
	return "", false
}

func nextPatternNumberIn(secrets map[string]string, prefix string) int {
	next := 0
	for placeholder := range secrets {
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

func (s *Store) collectPatternHits(text string) []patternHit {
	var hits []patternHit
	seen := make(map[patternHit]struct{})
	for _, configured := range s.patterns {
		for _, location := range configured.re.FindAllStringIndex(text, -1) {
			value := text[location[0]:location[1]]
			if len(value) < 8 {
				continue
			}
			hit := patternHit{index: configured.index, value: value}
			if _, ok := seen[hit]; ok {
				continue
			}
			seen[hit] = struct{}{}
			hits = append(hits, hit)
		}
	}
	return hits
}

func (s *Store) lookupPatternReplacements(text string) []replacement {
	var items []replacement
	for _, configured := range s.patterns {
		prefix := patternPrefix(configured.index)
		for _, location := range configured.re.FindAllStringIndex(text, -1) {
			value := text[location[0]:location[1]]
			if len(value) < 8 {
				continue
			}
			if placeholder, ok := findPatternPlaceholder(s.secrets, prefix, value); ok {
				items = append(items, replacement{from: value, to: placeholder})
			}
		}
	}
	return items
}

func (s *Store) syncPatternSecrets(hits []patternHit) error {
	unlock, err := lockAuth(s.dir)
	if err != nil {
		return err
	}
	syncErr := s.syncPatternSecretsLocked(hits)
	return errors.Join(syncErr, unlock())
}

func (s *Store) syncPatternSecretsLocked(hits []patternHit) error {
	disk, err := readSecretsMap(s.dir)
	if err != nil {
		return err
	}
	merged := mergeSecretMaps(disk, s.secrets)
	allocated := allocatePatternHits(merged, hits)
	if !allocated && maps.Equal(disk, merged) {
		s.secrets = merged
		return nil
	}
	if err := writeSecretsMap(s.dir, merged); err != nil {
		s.secrets = merged
		return err
	}
	s.secrets = merged
	return nil
}

func allocatePatternHits(secrets map[string]string, hits []patternHit) bool {
	allocated := false
	for _, hit := range hits {
		prefix := patternPrefix(hit.index)
		if _, ok := findPatternPlaceholder(secrets, prefix, hit.value); ok {
			continue
		}
		placeholder := prefix + strconv.Itoa(nextPatternNumberIn(secrets, prefix)) + "}}"
		secrets[placeholder] = hit.value
		allocated = true
	}
	return allocated
}

func mergeSecretMaps(disk, memory map[string]string) map[string]string {
	merged := make(map[string]string, len(disk)+len(memory))
	maps.Copy(merged, disk)
	for placeholder, secret := range memory {
		mergeOneSecret(merged, placeholder, secret)
	}
	return merged
}

func mergeOneSecret(merged map[string]string, placeholder, secret string) {
	if existing, ok := merged[placeholder]; ok && existing == secret {
		return
	}
	prefix, ok := placeholderPrefix(placeholder)
	if !ok {
		if _, exists := merged[placeholder]; !exists {
			merged[placeholder] = secret
		}
		return
	}
	if _, exists := merged[placeholder]; exists {
		if _, ok := findPatternPlaceholder(merged, prefix, secret); ok {
			return
		}
		fresh := prefix + strconv.Itoa(nextPatternNumberIn(merged, prefix)) + "}}"
		merged[fresh] = secret
		return
	}
	if _, ok := findPatternPlaceholder(merged, prefix, secret); ok {
		return
	}
	merged[placeholder] = secret
}

func readSecretsMap(dir string) (map[string]string, error) {
	raw, err := readSecure(filepath.Join(dir, "secrets.json"))
	if err != nil {
		return nil, err
	}
	if raw == nil {
		return map[string]string{}, nil
	}
	secrets := make(map[string]string)
	err = readObject(raw, func(placeholder string, value jsontext.Value) error {
		secret, decodeErr := jsonString(value)
		if decodeErr != nil {
			return fmt.Errorf("placeholder %q: %w", placeholder, decodeErr)
		}
		secrets[placeholder] = secret
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("secrets.json: %w", err)
	}
	return secrets, nil
}

func writeSecretsMap(dir string, secrets map[string]string) error {
	return writeSecure(filepath.Join(dir, "secrets.json"), func(encoder *jsontext.Encoder) error {
		return encodeSecrets(encoder, secrets)
	})
}

func encodeSecrets(encoder *jsontext.Encoder, secrets map[string]string) error {
	if err := encoder.WriteToken(jsontext.BeginObject); err != nil {
		return fmt.Errorf("write secrets object: %w", err)
	}
	for _, placeholder := range slices.Sorted(maps.Keys(secrets)) {
		if err := encoder.WriteToken(jsontext.String(placeholder)); err != nil {
			return fmt.Errorf("write secret placeholder: %w", err)
		}
		if err := encoder.WriteToken(jsontext.String(secrets[placeholder])); err != nil {
			return fmt.Errorf("write secret value: %w", err)
		}
	}
	if err := encoder.WriteToken(jsontext.EndObject); err != nil {
		return fmt.Errorf("finish secrets object: %w", err)
	}
	return nil
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
