package auth

import (
	"bytes"
	"errors"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

type LiveMask struct {
	store *Store
	view  []replacement
	bins  [256][]replacement
	scan  *secrets.ValuePrefix
}

func (s *Store) NewLiveMask() (*LiveMask, error) {
	view, err := s.liveReplacements()
	if err != nil {
		return nil, err
	}
	expressions := make([]string, 0, len(s.patterns))
	for _, configured := range s.patterns {
		expressions = append(expressions, configured.re.String())
	}
	scan, err := secrets.NewValuePrefix(expressions, s.builtin)
	if err != nil {
		return nil, err
	}
	live := &LiveMask{store: s, view: view, scan: scan}
	for _, item := range view {
		first := item.from[0]
		live.bins[first] = append(live.bins[first], item)
	}
	return live, nil
}

func (s *Store) liveReplacements() ([]replacement, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.reloadAuthIfChanged(); err != nil {
		return nil, err
	}
	if s.secretsDirty {
		return nil, errors.New(maskPersistenceFailure)
	}
	items := append(s.credentialReplacements(), s.recordedBuiltinReplacements(nil)...)
	slices.SortFunc(items, compareReplacements)
	return items, nil
}

func (m *LiveMask) SafeBytes(window []byte, base int64) int {
	return m.scan.SafeBytes(window, base)
}

func (m *LiveMask) MaskPrefix(window []byte, cut, limit int) (string, int, error) {
	current, err := m.store.liveReplacements()
	if err != nil {
		return "", 0, err
	}
	if !slices.Equal(m.view, current) {
		return "", 0, errors.New("secret masking state changed during exec")
	}
	text, used := m.replacePrefix(window, cut, limit)
	return text, used, nil
}

func (m *LiveMask) replacePrefix(window []byte, cut, limit int) (string, int) {
	var out strings.Builder
	position, written := 0, 0
	for position < cut {
		item := m.literalAt(window[position:])
		if item.from != "" {
			if position+len(item.from) > cut || out.Len()+position-written+len(item.to) > limit {
				break
			}
			_, _ = out.Write(window[written:position])
			out.WriteString(item.to)
			position += len(item.from)
			written = position
			continue
		}
		if out.Len()+position-written >= limit {
			break
		}
		position++
	}
	position = liveRuneCut(window, position, written)
	if written == 0 {
		return string(window[:position]), position
	}
	_, _ = out.Write(window[written:position])
	return out.String(), position
}

func liveRuneCut(window []byte, position, from int) int {
	floor := max(from, position-(utf8.UTFMax-1))
	for position > floor && position < len(window) && !utf8.RuneStart(window[position]) {
		position--
	}
	return position
}

func (m *LiveMask) literalAt(remaining []byte) replacement {
	for _, item := range m.bins[remaining[0]] {
		n := min(len(remaining), len(item.from))
		if bytes.Equal(remaining[:n], []byte(item.from[:n])) {
			return item
		}
	}
	return replacement{}
}
