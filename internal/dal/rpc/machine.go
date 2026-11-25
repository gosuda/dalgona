package rpc

import (
	"context"
	"encoding/binary"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"time"
)

const (
	MachinePreface = byte(0)
	MachineVersion = 1

	MachineIdentity = "identity"
	MachineSSH      = "ssh"
	MachineGrant    = "grant"
	MachineExchange = "exchange"
	MachineAdmit    = "admit"
	MachineDevices  = "devices"
	MachineRevoke   = "revoke"

	MachineMismatch    = "machine_mismatch"
	MachineDenied      = "credential_refused"
	MachineUnavailable = "pairing_unavailable"
	MachineInvalid     = "invalid_request"
	MachineMissing     = "device_not_found"

	MachineIdentityPath = "/pairing/v1/identity"
	MachineExchangePath = "/pairing/v1/exchange"
	MachineDevicePath   = "/pairing/v1/rpc"
	MachineIDHeader     = "Dalgona-Machine-ID"
	MachineFrameLimit   = 64<<10 - 1
	MachinePrefaceLimit = 1024
)

type PairingCard struct {
	Origin       string `json:"origin"`
	MachineID    string `json:"machineId"`
	PublicKeyPin string `json:"publicKeyPin"`
}

type PairedDevice struct {
	ID        string    `json:"id"`
	Label     string    `json:"label"`
	CreatedAt time.Time `json:"createdAt"`
}

type MachineRequest struct {
	Version    int    `json:"version"`
	Action     string `json:"action"`
	MachineID  string `json:"machineId,omitempty"`
	Credential string `json:"credential,omitempty"`
	Label      string `json:"label,omitempty"`
	DeviceID   string `json:"deviceId,omitempty"`
}

type MachineReply struct {
	Version    int            `json:"version"`
	MachineID  string         `json:"machineId,omitempty"`
	Status     string         `json:"status,omitempty"`
	Credential string         `json:"credential,omitempty"`
	Device     *PairedDevice  `json:"device,omitempty"`
	Devices    []PairedDevice `json:"devices,omitempty"`
}

func WriteMachineFrame(w io.Writer, value any) error {
	body, err := json.Marshal(value)
	if err != nil || len(body) > MachineFrameLimit {
		return errors.New("invalid machine frame")
	}
	var size [2]byte
	binary.BigEndian.PutUint16(size[:], uint16(len(body))) //nolint:gosec // G115: MachineFrameLimit fits in uint16
	if _, err := w.Write(size[:]); err != nil {
		return fmt.Errorf("write machine frame size: %w", err)
	}
	if _, err := w.Write(body); err != nil {
		return fmt.Errorf("write machine frame: %w", err)
	}
	return nil
}

func ReadMachineFrame(r io.Reader, limit int, value any) error {
	var size [2]byte
	if _, err := io.ReadFull(r, size[:]); err != nil {
		return fmt.Errorf("read machine frame size: %w", err)
	}
	n := int(binary.BigEndian.Uint16(size[:]))
	if n == 0 || n > limit {
		return errors.New("machine frame exceeds its size limit or is empty")
	}
	body := make([]byte, n)
	if _, err := io.ReadFull(r, body); err != nil {
		return fmt.Errorf("read machine frame: %w", err)
	}
	if err := json.Unmarshal(body, value, json.RejectUnknownMembers(true)); err != nil {
		return errors.New("invalid machine frame")
	}
	return nil
}

func ExchangeMachine(ctx context.Context, conn net.Conn, req MachineRequest) (MachineReply, error) {
	if err := ctx.Err(); err != nil {
		return MachineReply{}, err
	}
	deadline := time.Now().Add(5 * time.Second)
	if bound, ok := ctx.Deadline(); ok && bound.Before(deadline) {
		deadline = bound
	}
	if err := conn.SetDeadline(deadline); err != nil {
		return MachineReply{}, err
	}
	reply, err := exchangeMachine(conn, req)
	if err != nil || reply.Status != "" {
		return reply, err
	}
	if req.Action == MachineSSH || req.Action == MachineAdmit {
		return reply, conn.SetDeadline(time.Time{})
	}
	return reply, nil
}

func exchangeMachine(conn net.Conn, req MachineRequest) (reply MachineReply, err error) {
	req.Version = MachineVersion
	if _, err = conn.Write([]byte{MachinePreface}); err != nil {
		return reply, fmt.Errorf("write machine preface: %w", err)
	}
	if err = WriteMachineFrame(conn, req); err != nil {
		return reply, err
	}
	if err = ReadMachineFrame(conn, MachineFrameLimit, &reply); err != nil {
		return reply, err
	}
	if reply.Version != MachineVersion {
		return MachineReply{}, errors.New("unsupported daemon machine protocol; update the daemon")
	}
	return reply, nil
}

type deviceAdmissionKey struct{}

func WithDeviceAdmission(ctx context.Context, accepts func() bool) context.Context {
	return context.WithValue(ctx, deviceAdmissionKey{}, accepts)
}
