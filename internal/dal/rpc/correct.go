package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
)

type ResponseCheck struct {
	Accept func(result jsontext.Value) error
	Reject func(rejection error)
}

type ValidatedCall struct {
	ID     string
	Method string
	Params any
	Check  ResponseCheck
}

func (c *Conn) CallIDValidated(ctx context.Context, call ValidatedCall, result any) error {
	idv, err := json.Marshal(call.ID)
	if err != nil {
		return fmt.Errorf("encode request id: %w", err)
	}
	check := call.Check
	_, err = c.roundTrip(ctx, outgoing{id: idv, method: call.Method, params: call.Params, check: &check}, result)
	return err
}

func (pc *pendingCall) inspect(e envelope) error {
	if pc == nil || pc.check == nil || e.Error != nil {
		return nil
	}
	return pc.check.Accept(e.Result)
}
