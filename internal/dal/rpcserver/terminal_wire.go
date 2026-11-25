package rpcserver

import (
	"encoding/base64"
	"encoding/json/jsontext"
	"path/filepath"
	"runtime"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type terminalParams struct {
	seen         map[string]bool
	workspaceID  string
	terminalID   string
	attachmentID string
	checkpointID string
	access       string
	resumeLease  string
	writerLease  string
	dataBase64   string
	offset       string
	afterResize  string
	inputSeq     string
	resizeSeq    string
	columns      int
	rows         int
	limit        int
	waitMs       int
}

func decodeTerminalParams(params jsontext.Value, required ...string) (terminalParams, error) {
	p := terminalParams{seen: map[string]bool{}}
	text := map[string]*string{
		"workspaceId": &p.workspaceID, "terminalId": &p.terminalID, "attachmentId": &p.attachmentID,
		"checkpointId": &p.checkpointID, "access": &p.access, "resumeLease": &p.resumeLease, "writerLease": &p.writerLease,
		"dataBase64": &p.dataBase64, "offset": &p.offset, "afterResizeOrdinal": &p.afterResize,
		"inputSeq": &p.inputSeq, "resizeSeq": &p.resizeSeq,
	}
	numbers := map[string]*int{"columns": &p.columns, "rows": &p.rows, "limit": &p.limit, "waitMs": &p.waitMs}
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		p.seen[name] = true
		if field, ok := text[name]; ok {
			value, err := rpc.ReadString(d)
			*field = value
			return err
		}
		if field, ok := numbers[name]; ok {
			value, err := rpc.ReadInt(d)
			*field = value
			return err
		}
		return d.SkipValue()
	})
	if err != nil || !p.has(required...) || !p.identifiersBounded(text) {
		return terminalParams{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return p, nil
}

func (p terminalParams) has(names ...string) bool {
	for _, name := range names {
		if !p.seen[name] {
			return false
		}
	}
	return true
}

func (p terminalParams) identifiersBounded(text map[string]*string) bool {
	for _, name := range []string{"workspaceId", "terminalId", "attachmentId", "checkpointId", "resumeLease", "writerLease"} {
		value := *text[name]
		if p.seen[name] && (value == "" || len(value) > rpc.IDLimit) {
			return false
		}
	}
	return true
}

func (p terminalParams) size() (int, int, error) {
	if p.columns < terminalColumnsMin || p.columns > terminalColumnsMax || p.rows < terminalRowsMin || p.rows > terminalRowsMax {
		return 0, 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return p.columns, p.rows, nil
}

type numberRange struct {
	fallback int
	least    int
	most     int
}

func (p terminalParams) bounded(name string, value int, allowed numberRange) (int, error) {
	if !p.seen[name] {
		return allowed.fallback, nil
	}
	if value < allowed.least || value > allowed.most {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return value, nil
}

func (p terminalParams) cursor() (replayCursor, error) {
	offset, offsetErr := rpc.ParseDecimal(p.offset)
	ordinal, ordinalErr := rpc.ParseDecimal(p.afterResize)
	if offsetErr != nil || ordinalErr != nil {
		return replayCursor{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return replayCursor{offset: offset, ordinal: ordinal}, nil
}

func (p terminalParams) sequence(value string) (uint64, error) {
	seq, err := rpc.ParseDecimal(value)
	if err != nil || seq == 0 {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return seq, nil
}

func (p terminalParams) input() ([]byte, error) {
	data, err := base64.StdEncoding.DecodeString(p.dataBase64)
	if err != nil {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if len(data) > terminalInputLimit {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	return data, nil
}

func (p terminalParams) checkpointOffset() (int64, error) {
	if !p.seen["offset"] {
		return 0, nil
	}
	offset, err := rpc.ParseDecimal(p.offset)
	if err != nil || offset > checkpointBodyLimit {
		return 0, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return int64(offset), nil
}

func terminalSandbox(cfg config.Config, root string) runner.Sandbox {
	if cfg.Sandbox == config.SandboxOff {
		return runner.Sandbox{Mode: runner.SandboxOff}
	}
	sandbox := runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, WritableRoots: []string{root}}
	if runtime.GOOS != "windows" && cfg.Home != "" {
		sandbox.DeniedSockets = []string{filepath.Join(cfg.Home, "dalgona.sock")}
	}
	return sandbox
}

type attachmentView struct {
	id             string
	workspaceID    string
	writerLease    string
	acceptedInput  uint64
	acceptedResize uint64
	leaseExpiresAt string
}

func viewAttachment(att *attachment) attachmentView {
	view := attachmentView{id: att.id, workspaceID: att.workspaceID, leaseExpiresAt: rpc.FormatTimestamp(att.deadline)}
	if att.writer != nil {
		view.writerLease = att.writer.secret
		view.acceptedInput = att.writer.input.accepted
		view.acceptedResize = att.writer.resize.accepted
	}
	return view
}

func (v attachmentView) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("attachmentId", v.id)
	o.Str("workspaceId", v.workspaceID)
	o.OptStr("writerLease", v.writerLease)
	o.Str("acceptedInputSeq", rpc.FormatDecimal(v.acceptedInput))
	o.Str("acceptedResizeSeq", rpc.FormatDecimal(v.acceptedResize))
	o.Str("leaseExpiresAt", v.leaseExpiresAt)
	o.Close()
}

func encodeAttached(t *terminal, att *attachment) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Key("identity")
	t.identity.encode(o, t.id)
	o.Key("attachment")
	viewAttachment(att).Encode(o)
	o.Key("checkpoint")
	att.checkpoint.encode(o, t.host.engine)
	o.Key("status")
	t.statusLocked().Encode(o)
	o.Close()
	return o.Bytes()
}

func encodeRefreshed(t *terminal, cp *checkpoint) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Key("checkpoint")
	cp.encode(o, t.host.engine)
	o.Key("status")
	t.statusLocked().Encode(o)
	o.Close()
	return o.Bytes()
}

func encodePage(t *terminal, page replayPage, att *attachment) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Str("offset", rpc.FormatDecimal(page.offset))
	o.Str("next", rpc.FormatDecimal(page.next))
	o.Str("dataBase64", base64.StdEncoding.EncodeToString(page.data))
	o.Key("resizes")
	o.OpenArray()
	for _, record := range page.resizes {
		record.Encode(o)
	}
	o.CloseArray()
	o.Str("resizeOrdinal", rpc.FormatDecimal(page.ordinal))
	o.Key("status")
	t.statusLocked().Encode(o)
	o.Str("leaseExpiresAt", rpc.FormatTimestamp(att.deadline))
	o.Close()
	return o.Bytes()
}

func encodeAcceptedSeq(seq uint64) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Str("acceptedSeq", rpc.FormatDecimal(seq))
	o.Close()
	return o.Bytes()
}

func encodeResized(seq uint64, record terminalResize) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Str("acceptedSeq", rpc.FormatDecimal(seq))
	o.Str("ordinal", rpc.FormatDecimal(record.ordinal))
	o.Str("offset", rpc.FormatDecimal(record.offset))
	o.Int("columns", int64(record.columns))
	o.Int("rows", int64(record.rows))
	o.Close()
	return o.Bytes()
}

func encodeTerminalStatus(status terminalStatus) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Key("status")
	status.Encode(o)
	o.Close()
	return o.Bytes()
}

func encodeDetached(status string) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Str("status", status)
	o.Close()
	return o.Bytes()
}

type terminalListing struct {
	id            string
	identity      terminalIdentity
	status        terminalStatus
	writerPresent bool
	lease         string
}

type terminalListings []terminalListing

func (l terminalListings) Encode(o *rpc.Writer) {
	o.Open()
	o.Key("terminals")
	o.OpenArray()
	for _, entry := range l {
		o.Open()
		o.Key("identity")
		entry.identity.encode(o, entry.id)
		o.Key("status")
		entry.status.Encode(o)
		o.Bool("writerPresent", entry.writerPresent)
		o.Str("leaseExpiresAt", entry.lease)
		o.Close()
	}
	o.CloseArray()
	o.Close()
}
