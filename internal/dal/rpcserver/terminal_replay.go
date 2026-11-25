package rpcserver

import (
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type outputRing struct {
	buf   []byte
	start uint64
}

func (r *outputRing) next() uint64 { return r.start + uint64(len(r.buf)) }

func (r *outputRing) append(data []byte) {
	r.buf = append(r.buf, data...)
	if excess := len(r.buf) - terminalOutputRing; excess > 0 {
		r.buf = r.buf[excess:]
		r.start += uint64(excess)
	}
}

func (r *outputRing) slice(from, to uint64) []byte {
	return r.buf[from-r.start : to-r.start]
}

type terminalResize struct {
	ordinal uint64
	offset  uint64
	columns int
	rows    int
	size    int
}

func (r terminalResize) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("ordinal", rpc.FormatDecimal(r.ordinal))
	o.Str("offset", rpc.FormatDecimal(r.offset))
	o.Int("columns", int64(r.columns))
	o.Int("rows", int64(r.rows))
	o.Close()
}

func (r terminalResize) measured() terminalResize {
	r.size = terminalReadResizeBytes
	if value, err := rpc.Marshal(r); err == nil {
		r.size = len(value) + 1
	}
	return r
}

type resizeRing struct {
	records []terminalResize
	bytes   int
	evicted *terminalResize
}

func (r *resizeRing) latest() uint64 {
	if len(r.records) > 0 {
		return r.records[len(r.records)-1].ordinal
	}
	if r.evicted != nil {
		return r.evicted.ordinal
	}
	return 0
}

func (r *resizeRing) append(offset uint64, columns, rows int) terminalResize {
	record := terminalResize{ordinal: r.latest() + 1, offset: offset, columns: columns, rows: rows}.measured()
	r.records = append(r.records, record)
	r.bytes += record.size
	for len(r.records) > terminalResizeRecords || r.bytes > terminalResizeBytes {
		oldest := r.records[0]
		r.bytes -= oldest.size
		r.evicted = &oldest
		r.records = r.records[1:]
	}
	return record
}

func (r *resizeRing) find(ordinal uint64) (terminalResize, bool) {
	if r.evicted != nil && ordinal == r.evicted.ordinal {
		return *r.evicted, true
	}
	if len(r.records) == 0 || ordinal < r.records[0].ordinal {
		return terminalResize{}, false
	}
	index := ordinal - r.records[0].ordinal
	if index >= uint64(len(r.records)) {
		return terminalResize{}, false
	}
	return r.records[index], true
}

func (r *resizeRing) after(ordinal uint64) []terminalResize {
	if len(r.records) == 0 || ordinal >= r.latest() {
		return nil
	}
	return r.records[ordinal+1-r.records[0].ordinal:]
}

type replayCursor struct {
	offset  uint64
	ordinal uint64
}

func (r *resizeRing) validate(cursor replayCursor, output *outputRing) error {
	if err := r.checkBounds(cursor, output); err != nil {
		return err
	}
	return r.checkPosition(cursor)
}

func (r *resizeRing) checkBounds(cursor replayCursor, output *outputRing) error {
	if cursor.offset > output.next() || cursor.ordinal > r.latest() {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if cursor.offset < output.start || (r.evicted != nil && cursor.ordinal < r.evicted.ordinal) {
		return rpc.NativeKind(rpc.ErrorReplayGap)
	}
	return nil
}

func (r *resizeRing) checkPosition(cursor replayCursor) error {
	if applied, ok := r.find(cursor.ordinal); ok && applied.offset > cursor.offset {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if pending, ok := r.find(cursor.ordinal + 1); ok && pending.offset < cursor.offset {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return nil
}

type replayPage struct {
	offset  uint64
	next    uint64
	data    []byte
	resizes []terminalResize
	ordinal uint64
}

func (p replayPage) empty() bool { return p.next == p.offset && len(p.resizes) == 0 }

func assemblePage(output *outputRing, resizes *resizeRing, cursor replayCursor, limit int) (replayPage, error) {
	if limit < 0 {
		return replayPage{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	end := min(output.next(), cursor.offset+uint64(limit))
	page := replayPage{offset: cursor.offset, ordinal: cursor.ordinal}
	size := 0
	for _, record := range resizes.after(cursor.ordinal) {
		if record.offset > end {
			break
		}
		if len(page.resizes) == terminalReadResizes || size+record.size > terminalReadResizeBytes {
			end = record.offset
			break
		}
		page.resizes = append(page.resizes, record)
		page.ordinal = record.ordinal
		size += record.size
	}
	page.next = end
	page.data = output.slice(cursor.offset, end)
	return page, nil
}
