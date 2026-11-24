package rpc

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
)

type RequestArrival interface {
	RequestArrived(method string, params jsontext.Value)
}

func (c *Conn) announceRequest(e envelope) {
	if arrival, ok := c.clientHandler.(RequestArrival); ok {
		arrival.RequestArrived(e.Method, e.Params)
	}
}

func (c *Conn) Respond(ctx context.Context, requestID string, result any) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	idv, err := json.Marshal(requestID)
	if err != nil {
		return fmt.Errorf("encode request id: %w", err)
	}
	raw, err := json.Marshal(result)
	if err != nil {
		return fmt.Errorf("encode response to %s: %w", requestID, err)
	}
	return c.enqueue(envelope{JSONRPC: "2.0", ID: idv, Result: raw}, false)
}
