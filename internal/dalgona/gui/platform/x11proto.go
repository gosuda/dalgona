//go:build linux || freebsd || openbsd

package platform

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"io"
	"io/fs"
	"math/bits"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

const (
	x11OpCreateWindow         = 1
	x11OpDestroyWindow        = 4
	x11OpInternAtom           = 16
	x11OpChangeProperty       = 18
	x11OpDeleteProperty       = 19
	x11OpGetProperty          = 20
	x11OpConvertSelection     = 24
	x11OpTranslateCoordinates = 40

	x11PacketError = 0
	x11PacketReply = 1

	x11SetupFailed  = 0
	x11SetupSuccess = 1

	x11EventCodeMask        = 0x7f
	x11EventPropertyNotify  = 28
	x11EventSelectionNotify = 31

	x11PropertyNewValue   = 0
	x11ModeAppend         = 2
	x11WindowInputOnly    = 2
	x11ValueEventMask     = 0x00000800
	x11PropertyChangeMask = 0x00400000
	x11AtomString         = 31

	x11ReadUnits    = 1 << 16
	x11BufferSize   = 64 << 10
	x11SetupTimeout = 10 * time.Second
	x11TCPPort      = 6000
	x11SocketPrefix = "/tmp/.X11-unix/X"

	x11FamilyInternet  = 0
	x11FamilyInternet6 = 6
	x11FamilyLocal     = 256
	x11FamilyWild      = 65535
	x11CookieName      = "MIT-MAGIC-COOKIE-1"
)

var (
	errX11Malformed = errors.New("the X server sent a malformed packet; restart the X session, and report the server if this repeats")
	x11ErrorNames   = [...]string{1: "Request", "Value", "Window", "Pixmap", "Atom", "Cursor", "Font", "Match", "Drawable", "Access", "Alloc", "Colormap", "GContext", "IDChoice", "Name", "Length", "Implementation"}
	x11Loopback     = netip.AddrFrom4([4]byte{127, 0, 0, 1})
)

type x11Error struct {
	code  byte
	major byte
	value uint32
}

func (e *x11Error) Error() string {
	name := "code " + strconv.Itoa(int(e.code))
	if int(e.code) < len(x11ErrorNames) {
		name = x11ErrorNames[e.code]
	}
	return fmt.Sprintf("the X server rejected request %d with error %s for value %#x; try again, and report the error if it repeats", e.major, name, e.value)
}

type x11Display struct {
	network string
	address string
	number  string
	screen  uint64
}

func parseX11Display(name string) (x11Display, error) {
	colon := strings.LastIndexByte(name, ':')
	if colon < 0 || strings.ContainsRune(name, '/') {
		return x11Display{}, x11DisplayError(name)
	}
	host := name[:colon]
	digits, screenDigits, hasScreen := strings.Cut(name[colon+1:], ".")
	number, err := strconv.ParseUint(digits, 10, 16)
	if err != nil || number > 65535-x11TCPPort {
		return x11Display{}, x11DisplayError(name)
	}
	var screen uint64
	if hasScreen {
		if screen, err = strconv.ParseUint(screenDigits, 10, 8); err != nil {
			return x11Display{}, x11DisplayError(name)
		}
	}
	display := x11Display{network: "unix", number: strconv.FormatUint(number, 10), screen: screen}
	display.address = x11SocketPrefix + display.number
	if host != "" && host != "unix" {
		display.network = "tcp"
		display.address = net.JoinHostPort(strings.Trim(host, "[]"), strconv.FormatUint(x11TCPPort+number, 10))
	}
	return display, nil
}

func x11DisplayError(name string) error {
	return fmt.Errorf("the X11 display name %q is not [host]:display[.screen]; set DISPLAY to a display such as :0", name)
}

type x11AuthAddress struct {
	family  uint16
	address []byte
}

type x11AuthEntry struct {
	family  uint16
	address []byte
	number  []byte
	name    []byte
	data    []byte
}

func (e x11AuthEntry) grants(peer x11AuthAddress, number string) bool {
	host := e.family == x11FamilyWild || e.family == peer.family && bytes.Equal(e.address, peer.address)
	display := len(e.number) == 0 || string(e.number) == number
	return host && display && string(e.name) == x11CookieName
}

func nextX11AuthEntry(authority []byte) (x11AuthEntry, []byte, bool) {
	if len(authority) < 2 {
		return x11AuthEntry{}, nil, false
	}
	entry := x11AuthEntry{family: binary.BigEndian.Uint16(authority)}
	rest := authority[2:]
	for _, field := range [...]*[]byte{&entry.address, &entry.number, &entry.name, &entry.data} {
		if len(rest) < 2 {
			return x11AuthEntry{}, nil, false
		}
		size := 2 + int(binary.BigEndian.Uint16(rest))
		if len(rest) < size {
			return x11AuthEntry{}, nil, false
		}
		*field, rest = rest[2:size], rest[size:]
	}
	return entry, rest, true
}

func findX11Cookie(authority []byte, peer x11AuthAddress, number string) []byte {
	for {
		entry, rest, ok := nextX11AuthEntry(authority)
		if !ok {
			return nil
		}
		if entry.grants(peer, number) {
			return entry.data
		}
		authority = rest
	}
}

func x11AuthorityPath() string {
	if path := os.Getenv("XAUTHORITY"); path != "" {
		return path
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".Xauthority")
}

func x11PeerAddress(conn net.Conn) (x11AuthAddress, error) {
	if tcp, ok := conn.RemoteAddr().(*net.TCPAddr); ok {
		addr := tcp.AddrPort().Addr().Unmap()
		if addr != x11Loopback && addr != netip.IPv6Loopback() {
			family := uint16(x11FamilyInternet6)
			if addr.Is4() {
				family = x11FamilyInternet
			}
			return x11AuthAddress{family: family, address: addr.AsSlice()}, nil
		}
	}
	host, err := os.Hostname()
	if err != nil {
		return x11AuthAddress{}, fmt.Errorf("read this computer's host name to find its X authorization: %w; set the host name, then try again", err)
	}
	return x11AuthAddress{family: x11FamilyLocal, address: []byte(host)}, nil
}

func x11Cookie(conn net.Conn, number string) ([]byte, error) {
	path := x11AuthorityPath()
	if path == "" {
		return nil, nil
	}
	authority, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read the X authority file %s: %w; make it readable or set XAUTHORITY to the session's authority file", path, err)
	}
	peer, err := x11PeerAddress(conn)
	if err != nil {
		return nil, err
	}
	return findX11Cookie(authority, peer, number), nil
}

func x11Pad(b []byte) []byte {
	for len(b)%4 != 0 {
		b = append(b, 0)
	}
	return b
}

func x11SetupRequest(cookie []byte) []byte {
	request := []byte{'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0}
	if cookie == nil {
		return request
	}
	binary.LittleEndian.PutUint16(request[6:], uint16(len(x11CookieName)))
	binary.LittleEndian.PutUint16(request[8:], uint16(len(cookie))) //nolint:gosec // G115: an X authority entry stores its data length as a CARD16
	request = x11Pad(append(request, x11CookieName...))
	return x11Pad(append(request, cookie...))
}

type x11Setup struct {
	idBase          uint32
	idMask          uint32
	maxRequestUnits int
	root            uint32
}

func readX11Setup(in io.Reader, screen uint64) (x11Setup, error) {
	var head [8]byte
	if _, err := io.ReadFull(in, head[:]); err != nil {
		return x11Setup{}, fmt.Errorf("read the X server setup reply: %w; check that DISPLAY names a running X server", err)
	}
	body := make([]byte, 4*int(binary.LittleEndian.Uint16(head[6:])))
	if _, err := io.ReadFull(in, body); err != nil {
		return x11Setup{}, fmt.Errorf("read the X server setup reply: %w; check that DISPLAY names a running X server", err)
	}
	switch head[0] {
	case x11SetupSuccess:
		return parseX11Setup(body, screen)
	case x11SetupFailed:
		return x11Setup{}, x11Refused(body[:min(int(head[1]), len(body))])
	default:
		return x11Setup{}, x11Refused(bytes.TrimRight(body, "\x00"))
	}
}

func x11Refused(reason []byte) error {
	return fmt.Errorf("the X server refused the connection: %q; set XAUTHORITY to the authority file of the running X session, then try again", reason)
}

func parseX11Setup(body []byte, screen uint64) (x11Setup, error) {
	if len(body) < 32 {
		return x11Setup{}, errX11Malformed
	}
	setup := x11Setup{
		idBase:          binary.LittleEndian.Uint32(body[4:]),
		idMask:          binary.LittleEndian.Uint32(body[8:]),
		maxRequestUnits: int(binary.LittleEndian.Uint16(body[18:])),
	}
	offset := 32 + (int(binary.LittleEndian.Uint16(body[16:]))+3)&^3 + 8*int(body[21])
	for range screen {
		offset = x11ScreenEnd(body, offset)
	}
	if screen >= uint64(body[20]) || offset+40 > len(body) {
		return x11Setup{}, fmt.Errorf("the X server has no screen %d; set DISPLAY to an existing screen such as :0.0", screen)
	}
	setup.root = binary.LittleEndian.Uint32(body[offset:])
	return setup, nil
}

func x11ScreenEnd(body []byte, offset int) int {
	if offset+40 > len(body) {
		return len(body)
	}
	depths := int(body[offset+39])
	offset += 40
	for range depths {
		if offset+8 > len(body) {
			return len(body)
		}
		offset += 8 + 24*int(binary.LittleEndian.Uint16(body[offset+2:]))
	}
	return offset
}

type x11Conn struct {
	x11Setup
	conn     net.Conn
	in       *bufio.Reader
	out      []byte
	packet   []byte
	events   [][32]byte
	sequence uint16
	floor    uint16
	ids      uint32
}

type x11Property struct {
	kind   uint32
	format byte
	value  []byte
}

type x11PropertyWrite struct {
	window   uint32
	property uint32
	kind     uint32
	mode     byte
	format   byte
	data     []byte
}

func dialX11(name string) (*x11Conn, error) {
	display, err := parseX11Display(name)
	if err != nil {
		return nil, err
	}
	conn, err := net.DialTimeout(display.network, display.address, x11SetupTimeout)
	if err != nil {
		return nil, fmt.Errorf("connect to the X server of display %q: %w; start the X server or set DISPLAY to a running display", name, err)
	}
	c, err := x11Handshake(conn, display)
	if err != nil {
		return nil, errors.Join(err, conn.Close())
	}
	return c, nil
}

func x11Handshake(conn net.Conn, display x11Display) (*x11Conn, error) {
	cookie, err := x11Cookie(conn, display.number)
	if err != nil {
		return nil, err
	}
	if err = conn.SetDeadline(time.Now().Add(x11SetupTimeout)); err != nil {
		return nil, fmt.Errorf("bound the X server setup time: %w; try again", err)
	}
	if _, err = conn.Write(x11SetupRequest(cookie)); err != nil {
		return nil, fmt.Errorf("send the X connection setup: %w; check that DISPLAY names a running X server", err)
	}
	in := bufio.NewReaderSize(conn, x11BufferSize)
	setup, err := readX11Setup(in, display.screen)
	if err != nil {
		return nil, err
	}
	if err = conn.SetDeadline(time.Time{}); err != nil {
		return nil, fmt.Errorf("clear the X server setup deadline: %w; try again", err)
	}
	return &x11Conn{x11Setup: setup, conn: conn, in: in}, nil
}

func (c *x11Conn) close() error {
	return c.conn.Close()
}

func (c *x11Conn) mark() {
	c.floor = c.sequence + 1
}

func (c *x11Conn) interruptOn(ctx context.Context) func() {
	interrupted := make(chan struct{})
	stop := context.AfterFunc(ctx, func() {
		defer close(interrupted)
		_ = c.conn.SetDeadline(time.Unix(1, 0))
	})
	return func() {
		if !stop() {
			<-interrupted
			_ = c.conn.SetDeadline(time.Time{})
		}
	}
}

func (c *x11Conn) newID() uint32 {
	c.ids++
	return c.idBase | (c.ids<<bits.TrailingZeros32(c.idMask))&c.idMask
}

func (c *x11Conn) begin(opcode, detail byte) []byte {
	return append(c.out[:0], opcode, detail, 0, 0)
}

func (c *x11Conn) send(request []byte) error {
	request = x11Pad(request)
	units := len(request) / 4
	if units > c.maxRequestUnits {
		return fmt.Errorf("the X request of %d bytes exceeds the server's maximum of %d bytes; transfer the data in smaller parts", len(request), 4*c.maxRequestUnits)
	}
	binary.LittleEndian.PutUint16(request[2:], uint16(units)) //nolint:gosec // G115: units is at most the server's CARD16 maximum-request-length
	c.out = request[:0]
	c.sequence++
	if _, err := c.conn.Write(request); err != nil {
		return fmt.Errorf("send a request to the X server: %w", err)
	}
	return nil
}

func (c *x11Conn) roundTrip(request []byte) ([]byte, error) {
	if err := c.send(request); err != nil {
		return nil, err
	}
	return c.reply(c.sequence)
}

func (c *x11Conn) fill(size int) error {
	if cap(c.packet) < size {
		c.packet = append(make([]byte, 0, size), c.packet...)
	}
	for len(c.packet) < size {
		n, err := c.in.Read(c.packet[len(c.packet):size])
		c.packet = c.packet[:len(c.packet)+n]
		if err != nil {
			return fmt.Errorf("read from the X server: %w", err)
		}
	}
	return nil
}

func (c *x11Conn) readPacket() ([]byte, error) {
	if err := c.fill(32); err != nil {
		return nil, err
	}
	size := 32
	if c.packet[0] == x11PacketReply {
		extra := binary.LittleEndian.Uint32(c.packet[4:])
		if extra > x11ReadUnits {
			return nil, errX11Malformed
		}
		size += 4 * int(extra)
	}
	if err := c.fill(size); err != nil {
		return nil, err
	}
	packet := c.packet
	c.packet = c.packet[:0]
	return packet, nil
}

func (c *x11Conn) failure(packet []byte) error {
	sequence := binary.LittleEndian.Uint16(packet[2:])
	if sequence-c.floor > c.sequence-c.floor {
		return nil
	}
	return &x11Error{code: packet[1], major: packet[10], value: binary.LittleEndian.Uint32(packet[4:])}
}

func (c *x11Conn) reply(sequence uint16) ([]byte, error) {
	for {
		packet, err := c.readPacket()
		if err != nil {
			return nil, err
		}
		switch {
		case packet[0] == x11PacketError:
			if refusal := c.failure(packet); refusal != nil {
				return nil, refusal
			}
		case packet[0] != x11PacketReply:
			c.events = append(c.events, [32]byte(packet))
		case binary.LittleEndian.Uint16(packet[2:]) == sequence:
			return packet, nil
		}
	}
}

func (c *x11Conn) nextEvent() ([32]byte, error) {
	if len(c.events) > 0 {
		event := c.events[0]
		c.events = c.events[1:]
		return event, nil
	}
	for {
		packet, err := c.readPacket()
		if err != nil {
			return [32]byte{}, err
		}
		if packet[0] > x11PacketReply {
			return [32]byte(packet), nil
		}
		if packet[0] == x11PacketError {
			if refusal := c.failure(packet); refusal != nil {
				return [32]byte{}, refusal
			}
		}
	}
}

func (c *x11Conn) internAtoms(names []string) ([]uint32, error) {
	sequence := c.sequence
	for _, name := range names {
		request := c.begin(x11OpInternAtom, 0)
		request = binary.LittleEndian.AppendUint16(request, uint16(len(name))) //nolint:gosec // G115: send refuses any request beyond the CARD16 maximum-request-length, so a longer name never reaches the server
		request = append(request, 0, 0)
		if err := c.send(append(request, name...)); err != nil {
			return nil, err
		}
	}
	atoms := make([]uint32, len(names))
	for i := range atoms {
		sequence++
		reply, err := c.reply(sequence)
		if err != nil {
			return nil, err
		}
		atoms[i] = binary.LittleEndian.Uint32(reply[8:])
	}
	return atoms, nil
}

func (c *x11Conn) createInputWindow(eventMask uint32) (uint32, error) {
	window := c.newID()
	request := c.begin(x11OpCreateWindow, 0)
	request = binary.LittleEndian.AppendUint32(request, window)
	request = binary.LittleEndian.AppendUint32(request, c.root)
	request = append(request, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, x11WindowInputOnly, 0)
	request = binary.LittleEndian.AppendUint32(request, 0)
	request = binary.LittleEndian.AppendUint32(request, x11ValueEventMask)
	request = binary.LittleEndian.AppendUint32(request, eventMask)
	return window, c.send(request)
}

func (c *x11Conn) destroyWindow(window uint32) error {
	return c.send(binary.LittleEndian.AppendUint32(c.begin(x11OpDestroyWindow, 0), window))
}

func (c *x11Conn) changeProperty(w x11PropertyWrite) error {
	request := c.begin(x11OpChangeProperty, w.mode)
	request = binary.LittleEndian.AppendUint32(request, w.window)
	request = binary.LittleEndian.AppendUint32(request, w.property)
	request = binary.LittleEndian.AppendUint32(request, w.kind)
	request = append(request, w.format, 0, 0, 0)
	request = binary.LittleEndian.AppendUint32(request, uint32(len(w.data)/int(w.format/8))) //nolint:gosec // G115: send refuses any request beyond the CARD16 maximum-request-length, so the item count stays far below 2^32
	return c.send(append(request, w.data...))
}

func (c *x11Conn) deleteProperty(window, property uint32) error {
	request := binary.LittleEndian.AppendUint32(c.begin(x11OpDeleteProperty, 0), window)
	return c.send(binary.LittleEndian.AppendUint32(request, property))
}

func (c *x11Conn) convertSelection(requestor, selection, target, property, time uint32) error {
	request := c.begin(x11OpConvertSelection, 0)
	for _, field := range [...]uint32{requestor, selection, target, property, time} {
		request = binary.LittleEndian.AppendUint32(request, field)
	}
	return c.send(request)
}

func (c *x11Conn) readProperty(window, property uint32, into []byte) (x11Property, error) {
	read := x11Property{value: into}
	budget := int64(transferLimit - len(into))
	for {
		got := int64(len(read.value) - len(into))
		offset, length := got/4, min(x11ReadUnits, (budget-got)/4+1)
		request := c.begin(x11OpGetProperty, 0)
		for _, field := range [...]uint32{window, property, 0, uint32(offset), uint32(length)} { //nolint:gosec // G115: offset and length stay below 2^24 because the transfer budget is 32 MiB
			request = binary.LittleEndian.AppendUint32(request, field)
		}
		reply, err := c.roundTrip(request)
		if err != nil {
			return x11Property{}, err
		}
		read.kind, read.format = binary.LittleEndian.Uint32(reply[8:]), reply[1]
		size := int64(binary.LittleEndian.Uint32(reply[16:])) * int64(read.format/8)
		after := int64(binary.LittleEndian.Uint32(reply[12:]))
		switch {
		case size > int64(len(reply)-32):
			return x11Property{}, errX11Malformed
		case got+size+after > budget:
			return x11Property{}, ErrTransferTooLarge
		}
		read.value = append(read.value, reply[32:32+size]...)
		if after == 0 {
			return read, nil
		}
	}
}

func (c *x11Conn) windowOrigin(window uint32) (image.Point, error) {
	c.mark()
	request := binary.LittleEndian.AppendUint32(c.begin(x11OpTranslateCoordinates, 0), window)
	request = binary.LittleEndian.AppendUint32(request, c.root)
	reply, err := c.roundTrip(append(request, 0, 0, 0, 0))
	if err != nil {
		return image.Point{}, err
	}
	if reply[1] == 0 {
		return image.Point{}, fmt.Errorf("the X window %#x is on another screen than the display's default screen; set DISPLAY to the screen that shows the window", window)
	}
	return image.Pt(x11Int16(reply[12:]), x11Int16(reply[14:])), nil
}

func x11Int16(b []byte) int {
	v := int(binary.LittleEndian.Uint16(b))
	return v - v>>15<<16
}
