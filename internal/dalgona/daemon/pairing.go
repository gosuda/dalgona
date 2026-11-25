package daemon

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	grantLifetime        = 5 * time.Minute
	maxGrants            = 32
	maxDevices           = 64
	maxDeviceConnections = 8
	deviceTokenBytes     = 32
)

type Pairing struct {
	id       string
	path     string
	now      func() time.Time
	mu       sync.Mutex
	grants   map[[sha256.Size]byte]time.Time
	devices  map[string]*pairedDevice
	storeErr error
}

type deviceRecord struct {
	Device rpc.PairedDevice  `json:"device"`
	Digest [sha256.Size]byte `json:"digest"`
}

type pairedDevice struct {
	deviceRecord
	revoked atomic.Bool
	streams map[*deviceStream]struct{}
}

type deviceStream struct {
	conn   net.Conn
	cancel context.CancelFunc
}

func OpenPairing(home string) (*Pairing, error) {
	id, err := machineIdentity(home, createMachineSeed)
	if err != nil {
		return nil, err
	}
	p := &Pairing{
		id: id, path: filepath.Join(home, "devices-"+id+".json"), now: time.Now,
		grants: make(map[[sha256.Size]byte]time.Time), devices: make(map[string]*pairedDevice),
	}
	p.storeErr = p.load()
	return p, nil
}

func ReadMachineIdentity(home string) (string, error) {
	return machineIdentity(home, readMachineSeed)
}

func machineIdentity(home string, loadSeed func(string) ([sha256.Size]byte, error)) (string, error) {
	canonical, err := filepath.EvalSymlinks(home)
	if err != nil {
		return "", fmt.Errorf("canonical daemon home: %w", err)
	}
	canonical, err = filepath.Abs(canonical)
	if err != nil {
		return "", fmt.Errorf("absolute daemon home: %w", err)
	}
	seed, err := loadSeed(filepath.Join(canonical, "machine.json"))
	if err != nil {
		return "", fmt.Errorf("machine identity: %w", err)
	}
	h := sha256.New()
	_, _ = h.Write(seed[:])
	_, _ = io.WriteString(h, canonical)
	return hex.EncodeToString(h.Sum(nil)), nil
}

func readMachineSeed(path string) (seed [sha256.Size]byte, err error) {
	err = readPairingFile(path, &seed, 1024)
	if err == nil && seed == [sha256.Size]byte{} {
		err = errors.New("invalid machine identity")
	}
	return seed, err
}

func createMachineSeed(path string) ([sha256.Size]byte, error) {
	seed, err := readMachineSeed(path)
	if !errors.Is(err, fs.ErrNotExist) {
		return seed, err
	}
	if _, err := rand.Read(seed[:]); err != nil {
		return seed, fmt.Errorf("generate machine identity: %w", err)
	}
	return seed, writePairingFile(path, seed)
}

func readPairingFile(path string, value any, limit int64) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer func() { _ = file.Close() }()
	raw, err := io.ReadAll(io.LimitReader(file, limit+1))
	if err != nil {
		return err
	}
	if int64(len(raw)) > limit || json.Unmarshal(raw, value, json.RejectUnknownMembers(true)) != nil {
		return errors.New("invalid daemon pairing file")
	}
	return nil
}

func writePairingFile(path string, value any) error {
	return auth.WriteSecure(path, func(e *jsontext.Encoder) error { return json.MarshalEncode(e, value) })
}

func (p *Pairing) load() error {
	var records map[string]deviceRecord
	if err := readPairingFile(p.path, &records, 64<<10); err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	if len(records) > maxDevices {
		return errors.New("too many paired devices in daemon store")
	}
	for id, record := range records {
		if id == "" || id != record.Device.ID || record.Digest == [sha256.Size]byte{} || !validDeviceLabel(record.Device.Label) {
			return errors.New("invalid device in daemon pairing store")
		}
		p.devices[id] = newPairedDevice(record)
	}
	return nil
}

func newPairedDevice(record deviceRecord) *pairedDevice {
	return &pairedDevice{deviceRecord: record, streams: make(map[*deviceStream]struct{})}
}

func (p *Pairing) persist() bool {
	records := make(map[string]deviceRecord, len(p.devices))
	for id, d := range p.devices {
		records[id] = d.deviceRecord
	}
	p.storeErr = writePairingFile(p.path, records)
	if p.storeErr == nil {
		return true
	}
	for _, device := range p.devices {
		device.cutoff()
	}
	return false
}

func (d *pairedDevice) cutoff() {
	d.revoked.Store(true)
	for stream := range d.streams {
		stream.cancel()
		_ = stream.conn.Close()
	}
}

func (p *Pairing) grant() rpc.MachineReply {
	now := p.now()
	maps.DeleteFunc(p.grants, func(_ [sha256.Size]byte, expiry time.Time) bool { return !now.Before(expiry) })
	if len(p.grants) >= maxGrants || len(p.devices) >= maxDevices {
		return p.reply(rpc.MachineUnavailable)
	}
	code, err := pairingCredential()
	if err != nil {
		return p.reply(rpc.MachineUnavailable)
	}
	p.grants[sha256.Sum256([]byte(code))] = now.Add(grantLifetime)
	reply := p.reply("")
	reply.Credential = code
	return reply
}

func (p *Pairing) exchange(req rpc.MachineRequest) rpc.MachineReply {
	if req.MachineID != p.id {
		return p.reply(rpc.MachineMismatch)
	}
	if len(req.Credential) != base64.RawURLEncoding.EncodedLen(deviceTokenBytes) || !validDeviceLabel(req.Label) {
		return p.reply(rpc.MachineInvalid)
	}
	digest := sha256.Sum256([]byte(req.Credential))
	expires, present := p.grants[digest]
	now := p.now()
	if !present || !now.Before(expires) {
		return p.reply(rpc.MachineDenied)
	}
	if len(p.devices) >= maxDevices {
		return p.reply(rpc.MachineUnavailable)
	}
	delete(p.grants, digest)
	return p.issue(req.Label, now)
}

func (p *Pairing) issue(label string, now time.Time) rpc.MachineReply {
	token, err := pairingCredential()
	if err != nil {
		return p.reply(rpc.MachineUnavailable)
	}
	device := rpc.PairedDevice{ID: rand.Text(), Label: label, CreatedAt: now.UTC()}
	p.devices[device.ID] = newPairedDevice(deviceRecord{Device: device, Digest: sha256.Sum256([]byte(token))})
	if !p.persist() {
		return p.reply(rpc.MachineUnavailable)
	}
	reply := p.reply("")
	reply.Device = &device
	reply.Credential = token
	return reply
}

func pairingCredential() (string, error) {
	var random [deviceTokenBytes]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(random[:]), nil
}

func validDeviceLabel(label string) bool {
	return label != "" && len(label) <= 96 && strings.TrimSpace(label) == label && !strings.ContainsFunc(label, unicode.IsControl)
}

func (p *Pairing) list() rpc.MachineReply {
	reply := p.reply("")
	reply.Devices = make([]rpc.PairedDevice, 0, len(p.devices))
	for _, id := range slices.Sorted(maps.Keys(p.devices)) {
		reply.Devices = append(reply.Devices, p.devices[id].Device)
	}
	return reply
}

func (p *Pairing) revoke(id string) rpc.MachineReply {
	device, ok := p.devices[id]
	if !ok {
		return p.reply(rpc.MachineMissing)
	}
	device.revoked.Store(true)
	delete(p.devices, id)
	stored := p.persist()
	device.cutoff()
	if !stored {
		return p.reply(rpc.MachineUnavailable)
	}
	return p.reply("")
}

func (p *Pairing) reply(status string) rpc.MachineReply {
	return rpc.MachineReply{Version: rpc.MachineVersion, MachineID: p.id, Status: status}
}

func (p *Pairing) identity(expected string) rpc.MachineReply {
	if expected != "" && expected != p.id {
		return p.reply(rpc.MachineMismatch)
	}
	return p.reply("")
}

func (p *Pairing) control(req rpc.MachineRequest) rpc.MachineReply {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.storeErr != nil {
		return p.reply(rpc.MachineUnavailable)
	}
	switch req.Action {
	case rpc.MachineGrant:
		return p.grant()
	case rpc.MachineExchange:
		return p.exchange(req)
	case rpc.MachineDevices:
		return p.list()
	case rpc.MachineRevoke:
		return p.revoke(req.DeviceID)
	default:
		return p.reply(rpc.MachineInvalid)
	}
}

func (p *Pairing) admit(req rpc.MachineRequest, stream *deviceStream) (*pairedDevice, string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if req.MachineID != p.id {
		return nil, rpc.MachineMismatch
	}
	if p.storeErr != nil {
		return nil, rpc.MachineUnavailable
	}
	if len(req.Credential) != base64.RawURLEncoding.EncodedLen(deviceTokenBytes) {
		return nil, rpc.MachineDenied
	}
	digest := sha256.Sum256([]byte(req.Credential))
	var match *pairedDevice
	for _, device := range p.devices {
		if subtle.ConstantTimeCompare(digest[:], device.Digest[:]) == 1 {
			match = device
		}
	}
	if match == nil || match.revoked.Load() {
		return nil, rpc.MachineDenied
	}
	if len(match.streams) >= maxDeviceConnections {
		return nil, rpc.MachineUnavailable
	}
	match.streams[stream] = struct{}{}
	return match, ""
}

func (p *Pairing) release(device *pairedDevice, stream *deviceStream) {
	p.mu.Lock()
	defer p.mu.Unlock()
	delete(device.streams, stream)
}
