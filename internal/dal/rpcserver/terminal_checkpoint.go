package rpcserver

import (
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type checkpoint struct {
	id         string
	body       []byte
	digest     string
	nextOffset uint64
	ordinal    uint64
	columns    int
	rows       int
	expiresAt  time.Time
	reads      int
	retired    bool
}

func (t *terminal) captureLocked(now time.Time) (*checkpoint, error) {
	if t.screenFault != nil {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if err := t.host.chargeCheckpoint(checkpointBodyLimit); err != nil {
		return nil, err
	}
	body, err := t.screen.Checkpoint()
	if err != nil || len(body) > checkpointBodyLimit {
		t.host.refundCheckpoint(checkpointBodyLimit)
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	t.host.refundCheckpoint(checkpointBodyLimit - len(body))
	return &checkpoint{
		id: newHandle(), body: body, digest: digestOf(body), nextOffset: t.output.next(), ordinal: t.resizes.latest(),
		columns: t.columns, rows: t.rows, expiresAt: now.Add(checkpointLifetime),
	}, nil
}

func (t *terminal) refreshLocked(att *attachment, now time.Time) (*checkpoint, error) {
	if att.checkpoint != nil && att.checkpoint.reads > 0 {
		return nil, rpc.NativeKind(rpc.ErrorBusy)
	}
	cp, err := t.captureLocked(now)
	if err != nil {
		return nil, err
	}
	t.retireCheckpoint(att)
	att.checkpoint = cp
	t.renewLocked(att, now)
	return cp, nil
}

func (t *terminal) openCheckpointRead(att *attachment, checkpointID string, now time.Time) (*checkpoint, error) {
	cp := att.checkpoint
	if cp == nil || cp.id != checkpointID || !now.Before(cp.expiresAt) {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	cp.reads++
	t.renewLocked(att, now)
	return cp, nil
}

func (t *terminal) closeCheckpointRead(cp *checkpoint) {
	t.mu.Lock()
	defer t.mu.Unlock()
	cp.reads--
	if cp.retired && cp.reads == 0 {
		t.releaseCheckpoint(cp)
	}
}

func (t *terminal) retireCheckpoint(att *attachment) {
	cp := att.checkpoint
	if cp == nil {
		return
	}
	att.checkpoint = nil
	cp.retired = true
	if cp.reads == 0 {
		t.releaseCheckpoint(cp)
	}
}

func (t *terminal) releaseCheckpoint(cp *checkpoint) {
	t.host.refundCheckpoint(len(cp.body))
	cp.body = nil
}

func (c *checkpoint) encode(o *rpc.Writer, engine TerminalEngine) {
	o.Open()
	o.Str("checkpointId", c.id)
	o.Int("version", int64(engine.Version))
	o.Str("engineVersion", engine.EngineVersion)
	o.Str("nextOffset", rpc.FormatDecimal(c.nextOffset))
	o.Str("resizeOrdinal", rpc.FormatDecimal(c.ordinal))
	o.Int("columns", int64(c.columns))
	o.Int("rows", int64(c.rows))
	o.Str("total", rpc.FormatDecimal(uint64(len(c.body))))
	o.Str("digest", c.digest)
	o.Str("expiresAt", rpc.FormatTimestamp(c.expiresAt))
	o.Close()
}
