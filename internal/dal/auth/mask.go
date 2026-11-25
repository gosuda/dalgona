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

	"github.com/gosuda/dalgona/internal/dal/secrets"
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
	hits := s.collectHits(text, replacements)
	if err := s.persistMaskHits(hits); err != nil {
		return maskPersistenceFailure
	}
	replacements = append(replacements, s.lookupPatternReplacements(text)...)
	replacements = append(replacements, s.recordedBuiltinReplacements(func(value string) bool {
		return strings.Contains(text, value)
	})...)
	return applyReplacements(text, replacements)
}

func (s *Store) persistMaskHits(hits []patternHit) error {
	if len(hits) == 0 && !s.secretsDirty {
		return nil
	}
	if err := s.syncPatternSecrets(hits); err != nil {
		s.secretsDirty = true
		s.maskErr = fmt.Errorf("persist pattern secrets: %w", err)
		return s.maskErr
	}
	s.secretsDirty = false
	s.maskErr = nil
	return nil
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

func nextPlaceholder(text string, offset int) (int, int, bool) {
	for offset < len(text) {
		rel := strings.Index(text[offset:], "{{secret:")
		if rel < 0 {
			return 0, 0, false
		}
		start := offset + rel
		endRel := strings.Index(text[start:], "}}")
		if endRel < 0 {
			return 0, 0, false
		}
		end := start + endRel + 2
		if isSecretPlaceholder(text[start:end]) {
			return start, end, true
		}
		offset = start + 1
	}
	return 0, 0, false
}

func unresolvedPlaceholder(text string) (string, bool) {
	start, end, ok := nextPlaceholder(text, 0)
	if !ok {
		return "", false
	}
	return text[start:end], true
}

func placeholderSpans(text string) [][2]int {
	var spans [][2]int
	offset := 0
	for {
		start, end, ok := nextPlaceholder(text, offset)
		if !ok {
			return spans
		}
		spans = append(spans, [2]int{start, end})
		offset = end
	}
}

func placeholderBody(token string) (string, bool) {
	inner, ok := strings.CutPrefix(token, "{{secret:")
	if !ok {
		return "", false
	}
	return strings.CutSuffix(inner, "}}")
}

func isSecretPlaceholder(token string) bool {
	inner, ok := placeholderBody(token)
	if !ok || inner == "" {
		return false
	}
	if _, numbered := placeholderPrefix(token); numbered {
		return true
	}
	if strings.HasPrefix(inner, "auth:") {
		return isAuthPlaceholder(inner)
	}
	return isEnvPlaceholder(inner)
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
	if !SecretEnvironmentName(inner) {
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
	unlock, err := LockHome(s.dir)
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
	family string
	index  int
	value  string
}

const (
	familyPattern = "pattern"
	familyBuiltin = "builtin"
	familyKeyword = "keyword"
)

func patternPrefix(family string, index int) string {
	return "{{secret:" + family + strconv.Itoa(index) + ":"
}

func placeholderFamily(inner string) (string, bool) {
	for _, family := range [...]string{familyPattern, familyBuiltin, familyKeyword} {
		if strings.HasPrefix(inner, family) {
			return family, true
		}
	}
	return "", false
}

func validIndex(family, index string) bool {
	if family == familyKeyword {
		return index == ""
	}
	return isDecimalDigits(index)
}

func placeholderPrefix(placeholder string) (string, bool) {
	inner, ok := placeholderBody(placeholder)
	if !ok {
		return "", false
	}
	family, ok := placeholderFamily(inner)
	if !ok {
		return "", false
	}
	index, number, ok := strings.Cut(strings.TrimPrefix(inner, family), ":")
	if !ok || !isDecimalDigits(number) || !validIndex(family, index) {
		return "", false
	}
	return "{{secret:" + family + index + ":", true
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
			hit := patternHit{family: familyPattern, index: configured.index, value: value}
			if _, ok := seen[hit]; ok {
				continue
			}
			seen[hit] = struct{}{}
			hits = append(hits, hit)
		}
	}
	return hits
}

func (s *Store) collectHits(text string, covered []replacement) []patternHit {
	return slices.Concat(s.collectPatternHits(text), s.collectBuiltinHits(text, covered))
}

func (s *Store) collectBuiltinHits(text string, covered []replacement) []patternHit {
	if !s.builtin {
		return nil
	}
	spans := placeholderSpans(text)
	var hits []patternHit
	seen := make(map[patternHit]struct{})
	for _, found := range secrets.Scan(text) {
		hit := patternHit{family: familyBuiltin, index: found.Rule, value: text[found.Start:found.End]}
		_, duplicate := seen[hit]
		if duplicate || insidePlaceholder(spans, found) || isCovered(covered, hit.value) {
			continue
		}
		seen[hit] = struct{}{}
		hits = append(hits, hit)
	}
	return hits
}

func insidePlaceholder(spans [][2]int, found secrets.Hit) bool {
	return slices.ContainsFunc(spans, func(span [2]int) bool {
		return span[0] <= found.Start && found.End <= span[1]
	})
}

func isCovered(items []replacement, value string) bool {
	return slices.ContainsFunc(items, func(item replacement) bool { return item.from == value })
}

func (s *Store) lookupPatternReplacements(text string) []replacement {
	var items []replacement
	for _, configured := range s.patterns {
		prefix := patternPrefix(familyPattern, configured.index)
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

func (s *Store) recordedBuiltinReplacements(include func(string) bool) []replacement {
	var items []replacement
	for placeholder, value := range s.secrets {
		prefix, ok := placeholderPrefix(placeholder)
		if ok && strings.HasPrefix(prefix, "{{secret:"+familyBuiltin) && (include == nil || include(value)) {
			items = appendSecret(items, replacement{from: value, to: placeholder})
		}
	}
	return items
}

func (s *Store) syncPatternSecrets(hits []patternHit) error {
	unlock, err := LockHome(s.dir)
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
		prefix := patternPrefix(hit.family, hit.index)
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
	_, raw, err := readSecureInfo(filepath.Join(dir, "secrets.json"))
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
	return WriteSecure(filepath.Join(dir, "secrets.json"), func(encoder *jsontext.Encoder) error {
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
		if ok && SecretEnvironmentName(name) {
			items = appendSecret(items, replacement{from: value, to: "{{secret:" + name + "}}"})
		}
	}
	return items
}

func providerReplacements(provider string, credential Credential) []replacement {
	base := "{{secret:auth:" + provider
	keySuff, accSuff, refSuff := "}}", ":access}}", ":refresh}}"
	if credential.Key == "" {
		accSuff = "}}"
	}
	if credential.Key == "" && credential.Access == "" {
		refSuff = "}}"
	}
	var items []replacement
	if credential.Key != "" {
		items = appendSecret(items, replacement{from: credential.Key, to: base + keySuff})
	}
	if credential.Access != "" {
		items = appendSecret(items, replacement{from: credential.Access, to: base + accSuff})
	}
	if credential.Refresh != "" {
		items = appendSecret(items, replacement{from: credential.Refresh, to: base + refSuff})
	}
	return items
}

func appendSecret(items []replacement, item replacement) []replacement {
	if len(item.from) < 8 {
		return items
	}
	return append(items, item)
}

func SecretEnvironmentName(name string) bool {
	return strings.HasSuffix(name, "_API_KEY") || strings.HasSuffix(name, "_TOKEN") || strings.HasSuffix(name, "_SECRET") || strings.HasSuffix(name, "_PASSWORD")
}

func applyReplacements(text string, items []replacement) string {
	slices.SortFunc(items, compareReplacements)
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

func compareReplacements(a, b replacement) int {
	if len(a.from) != len(b.from) {
		return len(b.from) - len(a.from)
	}
	if a.from != b.from {
		return strings.Compare(a.from, b.from)
	}
	return strings.Compare(a.to, b.to)
}

func replacementAt(text string, items []replacement) (replacement, bool) {
	for _, item := range items {
		if strings.HasPrefix(text, item.from) {
			return item, true
		}
	}
	return replacement{}, false
}
