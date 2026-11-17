package journal

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/cockroachdb/pebble/v2"
)

type SearchQuery struct {
	Text  string
	Limit int
}

type SearchHit struct {
	Session string
	Seq     uint64
	Kind    string
	Body    string
	Matches int
}

var ErrQueryShort = errors.New("journal: search needs at least 3 characters")

const searchSessionCap = 1000

type searchFields struct {
	typ  string
	text string
	name string
}

func searchText(ev []byte) (kind, body string, ok bool) {
	decoder := jsontext.NewDecoder(bytes.NewReader(ev))
	fields, ok := decodeSearchFields(decoder)
	if !ok {
		return "", "", false
	}
	return searchKindBody(fields)
}

func decodeSearchFields(decoder *jsontext.Decoder) (searchFields, bool) {
	if decoder.PeekKind() != jsontext.KindBeginObject {
		return searchFields{}, false
	}
	open, err := decoder.ReadToken()
	if err != nil || open.Kind() != jsontext.KindBeginObject {
		return searchFields{}, false
	}
	var fields searchFields
	for decoder.PeekKind() != jsontext.KindEndObject {
		if !decodeSearchMember(decoder, &fields) {
			return searchFields{}, false
		}
	}
	return fields, true
}

func decodeSearchMember(decoder *jsontext.Decoder, fields *searchFields) bool {
	member, err := decoder.ReadToken()
	if err != nil {
		return false
	}
	name := member.String()
	var value string
	switch name {
	case "type", "text", "name":
		value, err = stringMember(decoder)
	default:
		err = decoder.SkipValue()
	}
	if err != nil {
		return false
	}
	switch name {
	case "type":
		fields.typ = value
	case "text":
		fields.text = value
	case "name":
		fields.name = value
	}
	return true
}

func searchKindBody(fields searchFields) (string, string, bool) {
	switch fields.typ {
	case "user.message":
		return "user", fields.text, fields.text != ""
	case "assistant.message":
		return "assistant", fields.text, fields.text != ""
	case "thread.named":
		return "title", fields.name, fields.name != ""
	}
	return "", "", false
}

func stringMember(d *jsontext.Decoder) (string, error) {
	token, err := d.ReadToken()
	if err != nil {
		return "", err
	}
	if token.Kind() != jsontext.KindString {
		return "", nil
	}
	return token.String(), nil
}

func searchMatch(text string) (string, error) {
	terms := strings.Fields(text)
	if len(terms) == 0 {
		return "", ErrQueryShort
	}
	quoted := make([]string, 0, len(terms))
	for _, term := range terms {
		if utf8.RuneCountInString(term) < 3 {
			return "", ErrQueryShort
		}
		quoted = append(quoted, `"`+strings.ReplaceAll(term, `"`, `""`)+`"`)
	}
	return strings.Join(quoted, " "), nil
}

func termMatches(body, text string) int {
	total := 0
	for term := range strings.FieldsSeq(text) {
		total += strings.Count(strings.ToLower(body), strings.ToLower(term))
	}
	return total
}

func (s *Store) Search(ctx context.Context, q SearchQuery) ([]SearchHit, error) {
	if _, err := searchMatch(q.Text); err != nil {
		return nil, err
	}
	s.mu.Lock()
	m := s.mirror
	s.mu.Unlock()
	if m == nil {
		return nil, errors.New("journal: search needs an open mirror")
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	rows, err := queryIndexRows(ctx, m, strings.Fields(q.Text))
	if err != nil {
		return nil, fmt.Errorf("journal: search: %w", err)
	}
	best, err := scanBestHits(rows, q.Text)
	if err != nil {
		return nil, err
	}
	return keepPersistent(ctx, m, rankedHits(best), searchLimit(q.Limit))
}

func rankedHits(best map[string]SearchHit) []SearchHit {
	hits := make([]SearchHit, 0, len(best))
	for _, hit := range best {
		hits = append(hits, hit)
	}
	slices.SortFunc(hits, func(a, b SearchHit) int {
		if a.Matches != b.Matches {
			return b.Matches - a.Matches
		}
		return strings.Compare(a.Session, b.Session)
	})
	return hits
}

func searchLimit(limit int) int {
	if limit > 0 && limit < searchSessionCap {
		return limit
	}
	return searchSessionCap
}

func keepPersistent(ctx context.Context, m *mirrorDB, ranked []SearchHit, limit int) ([]SearchHit, error) {
	kept := make([]SearchHit, 0, min(len(ranked), limit))
	err := m.withDB(func(db *pebble.DB) error {
		for _, hit := range ranked {
			if len(kept) == limit {
				break
			}
			if err := keepHit(ctx, db, hit, &kept); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return kept, nil
}

func keepHit(ctx context.Context, db *pebble.DB, hit SearchHit, kept *[]SearchHit) error {
	ephemeral, err := sessionEphemeral(ctx, db, hit.Session)
	if err != nil {
		return err
	}
	if !ephemeral {
		*kept = append(*kept, hit)
	}
	return nil
}

func sessionEphemeral(ctx context.Context, db *pebble.DB, session string) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}

	value, closer, err := db.Get(keyEvent(session, 1))
	if errors.Is(err, pebble.ErrNotFound) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("journal: search: %w", err)
	}
	defer func() { _ = closer.Close() }()
	_, ev, _ := bytes.Cut(value, []byte{0})
	return createdEphemeral(ev), nil
}

type createdFields struct {
	typ       string
	ephemeral bool
}

func createdEphemeral(ev []byte) bool {
	decoder := jsontext.NewDecoder(bytes.NewReader(ev))
	if decoder.PeekKind() != jsontext.KindBeginObject {
		return false
	}
	if _, err := decoder.ReadToken(); err != nil {
		return false
	}
	var fields createdFields
	for decoder.PeekKind() != jsontext.KindEndObject {
		if !decodeCreatedMember(decoder, &fields) {
			return false
		}
	}
	return fields.typ == "session.created" && fields.ephemeral
}

func decodeCreatedMember(decoder *jsontext.Decoder, fields *createdFields) bool {
	member, err := decoder.ReadToken()
	if err != nil {
		return false
	}
	switch member.String() {
	case "type":
		fields.typ, err = stringMember(decoder)
	case "ephemeral":
		fields.ephemeral, err = trueMember(decoder)
	default:
		err = decoder.SkipValue()
	}
	return err == nil
}

func trueMember(d *jsontext.Decoder) (bool, error) {
	if d.PeekKind() != jsontext.KindTrue {
		return false, d.SkipValue()
	}
	_, err := d.ReadToken()
	return true, err
}

func scanBestHits(rows []SearchHit, text string) (map[string]SearchHit, error) {
	best := map[string]SearchHit{}
	for _, hit := range rows {
		hit.Matches = termMatches(hit.Body, text)
		if current, seen := best[hit.Session]; !seen || hit.Matches > current.Matches {
			best[hit.Session] = hit
		}
	}
	return best, nil
}
