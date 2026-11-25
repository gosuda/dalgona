//go:build linux

package platform

import (
	"context"
	"fmt"
	"image"
	"os"
	"runtime/debug"
	"sync"
	"sync/atomic"
	"time"

	"github.com/godbus/dbus/v5"
)

const (
	atspiRegistryName      = "org.a11y.atspi.Registry"
	atspiSocket            = "org.a11y.atspi.Socket"
	atspiLauncherName      = "org.a11y.Bus"
	atspiLauncherPath      = dbus.ObjectPath("/org/a11y/bus")
	atspiCallTimeout       = 10 * time.Second
	atspiRetryFirst        = time.Second
	atspiRetryLast         = 30 * time.Second
	atspiAnnouncementLimit = 64
	atspiConnectFailure    = "connect to the accessibility bus %s: %w; restart at-spi-bus-launcher or correct AT_SPI_BUS_ADDRESS"
)

type atspiBridge struct {
	tree          *AccessibilityTree
	application   string
	locale        string
	version       string
	origin        func() image.Point
	bus           atomic.Pointer[atspiBus]
	index         atomic.Pointer[atspiIndex]
	detached      atomic.Bool
	wake          chan struct{}
	done          chan struct{}
	finished      chan struct{}
	mu            sync.Mutex
	latest        *SemanticSnapshot
	announcements []Announcement
}

type atspiBus struct {
	conn    *dbus.Conn
	name    string
	signals chan *dbus.Signal
	parent  atomic.Pointer[atspiRef]
	id      atomic.Int32
}

func AttachATSPI(tree *AccessibilityTree, application string, origin func() image.Point) (detach func(), err error) {
	b := &atspiBridge{
		tree:        tree,
		application: application,
		locale:      atspiLocale(),
		version:     atspiToolkitVersion(),
		origin:      origin,
		wake:        make(chan struct{}, 1),
		done:        make(chan struct{}),
		finished:    make(chan struct{}),
	}
	bus, err := b.open()
	if err != nil {
		return nil, err
	}
	stop := tree.Observe(b)
	go b.serve(bus, b.current())
	return sync.OnceFunc(func() { b.detach(stop) }), nil
}

func atspiLocale() string {
	for _, name := range []string{"LC_ALL", "LC_MESSAGES", "LANG"} {
		if value := os.Getenv(name); value != "" {
			return value
		}
	}
	return "C"
}

func atspiToolkitVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
	}
	return info.Main.Version
}

func atspiAddress(ctx context.Context) (string, error) {
	if address := os.Getenv("AT_SPI_BUS_ADDRESS"); address != "" {
		return address, nil
	}
	session, err := dbus.SessionBusPrivateNoAutoStartup()
	if err != nil {
		return "", fmt.Errorf("find the accessibility bus: the session bus is unavailable (%w); start a desktop session or set AT_SPI_BUS_ADDRESS", err)
	}
	defer session.Close()
	if err = atspiHandshake(ctx, session); err != nil {
		return "", fmt.Errorf("find the accessibility bus: the session bus refused the connection (%w); check DBUS_SESSION_BUS_ADDRESS", err)
	}
	var address string
	launcher := session.Object(atspiLauncherName, atspiLauncherPath)
	if err = launcher.CallWithContext(ctx, atspiLauncherName+".GetAddress", 0).Store(&address); err != nil {
		return "", fmt.Errorf("find the accessibility bus: %s did not answer GetAddress (%w); install at-spi2-core or set AT_SPI_BUS_ADDRESS", atspiLauncherName, err)
	}
	return address, nil
}

func atspiHandshake(ctx context.Context, conn *dbus.Conn) error {
	abort := context.AfterFunc(ctx, func() { _ = conn.Close() })
	err := conn.Auth(nil)
	if err == nil {
		err = conn.Hello()
	}
	if !abort() && err == nil {
		err = ctx.Err()
	}
	return err
}

func (b *atspiBridge) open() (*atspiBus, error) {
	ctx, cancel := context.WithTimeout(context.Background(), atspiCallTimeout)
	defer cancel()
	address, err := atspiAddress(ctx)
	if err != nil {
		return nil, err
	}
	conn, err := dbus.Dial(address, dbus.WithHandler(b))
	if err != nil {
		return nil, fmt.Errorf(atspiConnectFailure, address, err)
	}
	if err = atspiHandshake(ctx, conn); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf(atspiConnectFailure, address, err)
	}
	bus := &atspiBus{conn: conn, name: conn.Names()[0], signals: make(chan *dbus.Signal, 4)}
	b.bus.Store(bus)
	if err = bus.register(ctx); err != nil {
		_ = conn.Close()
		return nil, err
	}
	return bus, nil
}

func (bus *atspiBus) register(ctx context.Context) error {
	bus.conn.Signal(bus.signals)
	err := bus.conn.AddMatchSignalContext(ctx, dbus.WithMatchInterface(atspiSocket), dbus.WithMatchMember("Available"))
	if err != nil {
		return fmt.Errorf("watch the accessibility registry: the bus refused the signal match (%w); restart the accessibility bus", err)
	}
	if err = bus.embed(ctx); err != nil {
		return err
	}
	bus.emitCache("Ready")
	return nil
}

func (bus *atspiBus) embed(ctx context.Context) error {
	var parent atspiRef
	registry := bus.conn.Object(atspiRegistryName, atspiRootPath)
	if err := registry.CallWithContext(ctx, atspiSocket+".Embed", 0, bus.ref(atspiRootPath)).Store(&parent); err != nil {
		return fmt.Errorf("register with the accessibility registry: Embed failed (%w); start at-spi2-registryd or restart the accessibility bus", err)
	}
	bus.parent.Store(&parent)
	return nil
}

func (bus *atspiBus) ref(path dbus.ObjectPath) atspiRef {
	return atspiRef{Name: bus.name, Path: path}
}

func (b *atspiBridge) current() *SemanticSnapshot {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.latest = b.tree.Snapshot()
	return b.latest
}

func (b *atspiBridge) serve(bus *atspiBus, emitted *SemanticSnapshot) {
	defer close(b.finished)
	for bus != nil {
		select {
		case <-b.done:
			return
		case <-b.wake:
			emitted = b.flush(bus, emitted)
		case signal, open := <-bus.signals:
			bus.registryChanged(signal, open)
		case <-bus.conn.Context().Done():
			bus = b.reconnect()
			emitted = b.current()
		}
	}
}

func (bus *atspiBus) registryChanged(signal *dbus.Signal, open bool) {
	if !open {
		bus.signals = nil
		return
	}
	parent := bus.parent.Load()
	if signal.Name != atspiSocket+".Available" || (parent != nil && parent.Name == signal.Sender) {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), atspiCallTimeout)
	defer cancel()
	if err := bus.embed(ctx); err != nil {
		_ = bus.conn.Close()
	}
}

func (b *atspiBridge) reconnect() *atspiBus {
	delay := atspiRetryFirst
	for {
		timer := time.NewTimer(delay)
		select {
		case <-b.done:
			timer.Stop()
			return nil
		case <-timer.C:
		}
		if bus, err := b.open(); err == nil {
			return bus
		}
		delay = min(2*delay, atspiRetryLast)
	}
}

func (b *atspiBridge) detach(stop func()) {
	stop()
	close(b.done)
	<-b.finished
	b.detached.Store(true)
	bus := b.bus.Load()
	registry := bus.conn.Object(atspiRegistryName, atspiRootPath)
	registry.Go(atspiSocket+".Unembed", dbus.FlagNoReplyExpected, nil, bus.ref(atspiRootPath))
	_ = bus.conn.Close()
}

func (b *atspiBridge) screenOrigin() image.Point {
	if b.origin == nil {
		return image.Point{}
	}
	return b.origin()
}

func (b *atspiBridge) indexOf(snap *SemanticSnapshot) *atspiIndex {
	if cached := b.index.Load(); cached != nil && cached.snap == snap {
		return cached
	}
	index := newATSPIIndex(snap)
	b.index.Store(index)
	return index
}
