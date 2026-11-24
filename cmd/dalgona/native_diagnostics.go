package main

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"net"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/rpcserver"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/doctor"
)

const (
	methodDiagnosticsRun = "dalgona/diagnostics/run"
	diagnosticRowLimit   = 1000
	diagnosticByteLimit  = 1 << 20
)

type diagnostics []doctor.Row

func diagnosticsExtensions(home string) map[string]rpcserver.Extension {
	run := func(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
		online, err := decodeDiagnosticsRun(params)
		if err != nil {
			return nil, err
		}
		rows := doctor.Diagnose(ctx, doctor.Env{
			Home:   home,
			Dial:   func(context.Context) (net.Conn, error) { return client.DialHome(home) },
			Own:    config.Build(),
			Now:    time.Now,
			Online: online,
		})
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		return encodeDiagnostics(rows)
	}
	return map[string]rpcserver.Extension{methodDiagnosticsRun: rpcserver.WithDeadline(rpcserver.PageDeadline, run)}
}

func decodeDiagnosticsRun(params jsontext.Value) (bool, error) {
	var online *bool
	err := decodeMembers(params, func(name string, d *jsontext.Decoder) error {
		if name != "online" {
			return d.SkipValue()
		}
		value, readErr := rpc.ReadBool(d)
		online = &value
		return readErr
	})
	if err != nil || online == nil {
		return false, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return *online, nil
}

func decodeMembers(params jsontext.Value, member rpc.MemberFunc) error {
	if len(params) == 0 {
		return nil
	}
	return rpc.ReadMembers(jsontext.NewDecoder(bytes.NewReader(params)), member)
}

func encodeDiagnostics(rows []doctor.Row) (jsontext.Value, error) {
	if len(rows) > diagnosticRowLimit {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	encoded, err := rpc.Marshal(diagnostics(rows))
	if err != nil {
		return nil, err
	}
	if len(encoded) > diagnosticByteLimit {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	return encoded, nil
}

func (rows diagnostics) Encode(o *rpc.Writer) {
	o.Open()
	o.Key("rows")
	o.OpenArray()
	for _, row := range rows {
		o.Open()
		o.Str("status", row.Status)
		o.Str("check", row.Check)
		o.Str("detail", row.Detail)
		o.Close()
	}
	o.CloseArray()
	o.Int("exitCode", int64(doctor.ExitCode(rows)))
	o.Close()
}
