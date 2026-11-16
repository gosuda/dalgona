package journal

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"hash/crc32"
	"io"
)

const (
	maxLine       = 16 << 20
	genesisPrev   = "0000000000000000000000000000000000000000000000000000000000000000"
	hexHashLen    = 64
	hexCRCLen     = 8
	envelopeHead  = `{"seq":`
	envelopeMid1  = `,"prev":"`
	envelopeMid2  = `","hash":"`
	envelopeMid3  = `","crc":"`
	envelopeEv    = `","ev":`
	envelopeClose = '}'
)

var castagnoli = crc32.MakeTable(crc32.Castagnoli)

type lineKind uint8

const (
	lineOK lineKind = iota
	lineParse
	lineCRC
	lineHash
	lineSeq
	lineLink
)

type record struct {
	seq     uint64
	prev    string
	hash    string
	crc     uint32
	ev      []byte
	kind    lineKind
	lineOff int64
	lineEnd int64
}

type parsedEnvelope struct {
	seq  uint64
	prev string
	hash string
	crc  uint32
	ev   []byte
}

type envelopeParser struct {
	line []byte
	pos  int
}

func hashRecord(prev string, seq uint64, ev []byte) [32]byte {
	h := sha256.New()
	_, _ = h.Write([]byte(prev))
	var seqBuf [8]byte
	binary.BigEndian.PutUint64(seqBuf[:], seq)
	_, _ = h.Write(seqBuf[:])
	_, _ = h.Write(ev)
	var sum [32]byte
	copy(sum[:], h.Sum(nil))
	return sum
}

func parseHex32(s string) ([32]byte, bool) {
	var out [32]byte
	if len(s) != hexHashLen {
		return out, false
	}
	if _, err := hex.Decode(out[:], []byte(s)); err != nil {
		return out, false
	}
	return out, true
}

func parseSeq(s []byte, i *int) (uint64, bool) {
	start := *i
	var n uint64
	for *i < len(s) && s[*i] >= '0' && s[*i] <= '9' {
		d := uint64(s[*i] - '0')
		if n > (^uint64(0)-d)/10 {
			return 0, false
		}
		n = n*10 + d
		*i++
	}
	return n, *i > start
}

func (p *envelopeParser) take(lit string) bool {
	if len(p.line)-p.pos < len(lit) || !bytes.Equal(p.line[p.pos:p.pos+len(lit)], []byte(lit)) {
		return false
	}
	p.pos += len(lit)
	return true
}

func (p *envelopeParser) hexField(n int) (string, bool) {
	if len(p.line)-p.pos < n {
		return "", false
	}
	field := p.line[p.pos : p.pos+n]
	for _, c := range field {
		if !isLowerHex(c) {
			return "", false
		}
	}
	p.pos += n
	return string(field), true
}

func isLowerHex(c byte) bool {
	return c >= '0' && c <= '9' || c >= 'a' && c <= 'f'
}

func parseCRC(s string) (uint32, bool) {
	if len(s) != hexCRCLen {
		return 0, false
	}
	var value uint32
	for i := range len(s) {
		value = value<<4 | uint32(hexNibble(s[i]))
	}
	return value, true
}

func hexNibble(c byte) byte {
	if c <= '9' {
		return c - '0'
	}
	return c - 'a' + 10
}

func parseEnvelope(line []byte) (parsedEnvelope, bool) {
	p := envelopeParser{line: line}
	var out parsedEnvelope
	if !p.take(envelopeHead) {
		return out, false
	}
	seq, ok := parseSeq(line, &p.pos)
	if !ok {
		return out, false
	}
	out.seq = seq
	if !p.parseHashes(&out) || !p.parseCRCAndEvent(&out) {
		return parsedEnvelope{}, false
	}
	if len(line) == 0 || line[len(line)-1] != byte(envelopeClose) {
		return parsedEnvelope{}, false
	}
	out.ev = line[p.pos : len(line)-1]
	return out, true
}

func (p *envelopeParser) parseHashes(out *parsedEnvelope) bool {
	if !p.take(envelopeMid1) {
		return false
	}
	prev, ok := p.hexField(hexHashLen)
	if !ok || !p.take(envelopeMid2) {
		return false
	}
	hash, ok := p.hexField(hexHashLen)
	if !ok || !p.take(envelopeMid3) {
		return false
	}
	out.prev, out.hash = prev, hash
	return true
}

func (p *envelopeParser) parseCRCAndEvent(out *parsedEnvelope) bool {
	crcText, ok := p.hexField(hexCRCLen)
	if !ok {
		return false
	}
	crc, ok := parseCRC(crcText)
	if !ok || !p.take(envelopeEv) {
		return false
	}
	out.crc = crc
	return true
}

func validEventJSON(ev []byte) error {
	if bytes.ContainsAny(ev, "\r\n") {
		return errors.New("journal: event must be single-line JSON")
	}
	d := jsontext.NewDecoder(bytes.NewReader(ev))
	if d.PeekKind() != jsontext.KindBeginObject {
		return errors.New("journal: event must be a JSON object")
	}
	if _, err := d.ReadToken(); err != nil {
		return fmt.Errorf("journal: invalid event JSON: %w", err)
	}
	found, err := scanEventObject(d)
	if err != nil {
		return err
	}
	if !found {
		return errors.New("journal: event needs a string type field")
	}
	return requireJSONEOF(d)
}

func scanEventObject(d *jsontext.Decoder) (bool, error) {
	found := false
	for {
		tok, err := d.ReadToken()
		if err != nil {
			return false, fmt.Errorf("journal: invalid event JSON: %w", err)
		}
		if tok.Kind() == jsontext.KindEndObject {
			return found, nil
		}
		isType := tok.String() == "type" && d.PeekKind() == jsontext.KindString
		if err := d.SkipValue(); err != nil {
			return false, fmt.Errorf("journal: invalid event JSON: %w", err)
		}
		found = found || isType
	}
}

func requireJSONEOF(d *jsontext.Decoder) error {
	_, err := d.ReadToken()
	if errors.Is(err, io.EOF) {
		return nil
	}
	return errors.New("journal: event must be a single JSON object")
}

func verifyRecord(line []byte, wantSeq uint64, wantPrev string) record {
	r := record{kind: lineParse}
	if len(line)+1 > maxLine {
		return r
	}
	parsed, ok := parseEnvelope(line)
	if !ok || validEventJSON(parsed.ev) != nil {
		return r
	}
	r.seq, r.prev, r.hash, r.crc, r.ev = parsed.seq, parsed.prev, parsed.hash, parsed.crc, parsed.ev
	if parsed.seq != wantSeq {
		r.kind = lineSeq
		return r
	}
	if crc32.Checksum(parsed.ev, castagnoli) != parsed.crc {
		r.kind = lineCRC
		return r
	}
	sum := hashRecord(parsed.prev, parsed.seq, parsed.ev)
	want, ok := parseHex32(parsed.hash)
	if !ok || !bytes.Equal(want[:], sum[:]) {
		r.kind = lineHash
		return r
	}
	if parsed.prev != wantPrev {
		r.kind = lineLink
		return r
	}
	r.kind = lineOK
	return r
}

func encodeLine(buf []byte, seq uint64, prev, hash string, ev []byte) []byte {
	buf = append(buf, envelopeHead...)
	buf = appendUint(buf, seq)
	buf = append(buf, envelopeMid1...)
	buf = append(buf, prev...)
	buf = append(buf, envelopeMid2...)
	buf = append(buf, hash...)
	buf = append(buf, envelopeMid3...)
	buf = appendHex32(buf, crc32.Checksum(ev, castagnoli))
	buf = append(buf, envelopeEv...)
	buf = append(buf, ev...)
	buf = append(buf, envelopeClose, '\n')
	return buf
}

func appendUint(buf []byte, n uint64) []byte {
	var tmp [20]byte
	i := len(tmp)
	for {
		i--
		tmp[i] = byte('0' + n%10)
		n /= 10
		if n == 0 {
			break
		}
	}
	return append(buf, tmp[i:]...)
}

func appendHex32(buf []byte, v uint32) []byte {
	const digits = "0123456789abcdef"
	var tmp [8]byte
	for i := 7; i >= 0; i-- {
		tmp[i] = digits[v&0xf]
		v >>= 4
	}
	return append(buf, tmp[:]...)
}

func hexOfSum(sum [32]byte) string {
	var out [hexHashLen]byte
	hex.Encode(out[:], sum[:])
	return string(out[:])
}

func hexDecode(value string) ([]byte, error) {
	out := make([]byte, hex.DecodedLen(len(value)))
	_, err := hex.Decode(out, []byte(value))
	return out, err
}

func splitLines(data []byte) []record {
	var lines []record
	off := 0
	for off < len(data) {
		idx := bytes.IndexByte(data[off:], '\n')
		end := len(data)
		if idx >= 0 {
			end = off + idx
		}
		lines = append(lines, record{lineOff: int64(off), lineEnd: int64(end)})
		if idx < 0 {
			break
		}
		off = end + 1
	}
	return lines
}
