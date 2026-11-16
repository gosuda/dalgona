package journal

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"
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
	match, err := searchMatch(q.Text)
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	db := s.db
	s.mu.Unlock()
	if db == nil {
		return nil, errors.New("journal: search needs an open mirror")
	}
	rows, err := db.QueryContext(ctx, `SELECT session, seq, kind, body FROM thread_fts WHERE thread_fts MATCH ? ORDER BY session, seq`, match)
	if err != nil {
		return nil, fmt.Errorf("journal: search: %w", err)
	}
	best, err := scanBestHits(rows, q.Text)
	if err != nil {
		return nil, err
	}
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
	limit := searchSessionCap
	if q.Limit > 0 && q.Limit < limit {
		limit = q.Limit
	}
	return keepPersistent(ctx, db, hits, limit)
}

func keepPersistent(ctx context.Context, db *sql.DB, ranked []SearchHit, limit int) ([]SearchHit, error) {
	kept := make([]SearchHit, 0, min(len(ranked), limit))
	for _, hit := range ranked {
		if len(kept) == limit {
			break
		}
		ephemeral, err := sessionEphemeral(ctx, db, hit.Session)
		if err != nil {
			return nil, err
		}
		if !ephemeral {
			kept = append(kept, hit)
		}
	}
	return kept, nil
}

func sessionEphemeral(ctx context.Context, db *sql.DB, session string) (bool, error) {
	var ev []byte
	err := db.QueryRowContext(ctx, `SELECT ev FROM events WHERE session = ? AND seq = 1`, session).Scan(&ev)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("journal: search: %w", err)
	}
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

func scanBestHits(rows *sql.Rows, text string) (map[string]SearchHit, error) {
	best := map[string]SearchHit{}
	for rows.Next() {
		var hit SearchHit
		if err := rows.Scan(&hit.Session, &hit.Seq, &hit.Kind, &hit.Body); err != nil {
			return nil, errors.Join(fmt.Errorf("journal: search: %w", err), rows.Close())
		}
		hit.Matches = termMatches(hit.Body, text)
		if current, seen := best[hit.Session]; !seen || hit.Matches > current.Matches {
			best[hit.Session] = hit
		}
	}
	if err := rows.Err(); err != nil {
		return nil, errors.Join(fmt.Errorf("journal: search: %w", err), rows.Close())
	}
	if err := rows.Close(); err != nil {
		return nil, fmt.Errorf("journal: search: %w", err)
	}
	return best, nil
}
