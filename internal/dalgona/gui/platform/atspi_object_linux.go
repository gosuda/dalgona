//go:build linux

package platform

import (
	"encoding/hex"
	"fmt"
	"slices"
	"strings"

	"github.com/godbus/dbus/v5"
)

const (
	atspiRootPath     = dbus.ObjectPath("/org/a11y/atspi/accessible/root")
	atspiFramePath    = dbus.ObjectPath("/org/a11y/atspi/accessible/frame")
	atspiCachePath    = dbus.ObjectPath("/org/a11y/atspi/cache")
	atspiNullPath     = dbus.ObjectPath("/org/a11y/atspi/null")
	atspiNodePrefix   = "/org/a11y/atspi/accessible/node/"
	atspiAccessible   = "org.a11y.atspi.Accessible"
	atspiApplication  = "org.a11y.atspi.Application"
	atspiComponent    = "org.a11y.atspi.Component"
	atspiAction       = "org.a11y.atspi.Action"
	atspiText         = "org.a11y.atspi.Text"
	atspiEditableText = "org.a11y.atspi.EditableText"
	atspiSelection    = "org.a11y.atspi.Selection"
	atspiTable        = "org.a11y.atspi.Table"
	atspiTableCell    = "org.a11y.atspi.TableCell"
	atspiCache        = "org.a11y.atspi.Cache"
	atspiProperties   = "org.freedesktop.DBus.Properties"
	atspiHex          = "0123456789abcdef"
)

var atspiFixedPaths = map[dbus.ObjectPath]atspiKind{
	atspiRootPath:  atspiApplicationKind,
	atspiFramePath: atspiFrameKind,
	atspiCachePath: atspiCacheKind,
}

type atspiRef struct {
	Name string
	Path dbus.ObjectPath
}

type atspiObject struct {
	bridge *atspiBridge
	bus    *atspiBus
	snap   *SemanticSnapshot
	node   SemanticNode
	kind   atspiKind
}

type atspiMember struct {
	in   []any
	out  []any
	call func(o *atspiObject, args []any) ([]any, error)
}

type atspiProperty func(o *atspiObject) (any, error)

type atspiInterface struct {
	members    map[string]atspiMember
	properties map[string]atspiProperty
}

type atspiBound struct {
	object  *atspiObject
	members map[string]atspiMember
	text    bool
}

type atspiMethod struct {
	object *atspiObject
	member atspiMember
	text   bool
}

type atspiStale struct{}

func atspiNodePath(id NodeID) dbus.ObjectPath {
	var path strings.Builder
	path.Grow(len(atspiNodePrefix) + 3*len(id))
	path.WriteString(atspiNodePrefix)
	for i := range len(id) {
		c := id[i]
		if atspiPlain(c) {
			path.WriteByte(c)
			continue
		}
		path.WriteByte('_')
		path.WriteByte(atspiHex[c>>4])
		path.WriteByte(atspiHex[c&0xf])
	}
	return dbus.ObjectPath(path.String())
}

func atspiPlain(c byte) bool {
	return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9'
}

func atspiNodeID(path dbus.ObjectPath) (NodeID, bool) {
	element, ok := strings.CutPrefix(string(path), atspiNodePrefix)
	if !ok || element == "" {
		return "", false
	}
	id := make([]byte, 0, len(element))
	for i := 0; i < len(element); i++ {
		if element[i] != '_' {
			id = append(id, element[i])
			continue
		}
		if i+3 > len(element) {
			return "", false
		}
		decoded, err := hex.DecodeString(element[i+1 : i+3])
		if err != nil {
			return "", false
		}
		id = append(id, decoded...)
		i += 2
	}
	return NodeID(id), atspiNodePath(NodeID(id)) == path
}

func (b *atspiBridge) LookupObject(path dbus.ObjectPath) (dbus.ServerObject, bool) {
	bus := b.bus.Load()
	if bus == nil || b.detached.Load() {
		return nil, false
	}
	base := &atspiObject{bridge: b, bus: bus, snap: b.tree.Snapshot()}
	if kind, fixed := atspiFixedPaths[path]; fixed {
		return base.object(kind, SemanticNode{}), true
	}
	id, ok := atspiNodeID(path)
	if !ok {
		return nil, false
	}
	if node, found := base.lookup(id); found {
		return base.object(atspiNodeKind, node), true
	}
	return atspiStale{}, true
}

func (o *atspiObject) lookup(id NodeID) (SemanticNode, bool) {
	if o.snap == nil {
		return SemanticNode{}, false
	}
	return o.snap.Node(id)
}

func (o *atspiObject) LookupInterface(name string) (dbus.Interface, bool) {
	if name == atspiProperties {
		return atspiBound{object: o, members: atspiPropertyMembers}, true
	}
	if !slices.Contains(o.interfaces(), name) {
		return nil, false
	}
	return atspiBound{object: o, members: atspiInterfaces[name].members, text: name == atspiText}, true
}

func (i atspiBound) LookupMethod(name string) (dbus.Method, bool) {
	member, ok := i.members[name]
	return atspiMethod{object: i.object, member: member, text: i.text}, ok
}

func (m atspiMethod) DecodeArguments(_ *dbus.Conn, _ string, msg *dbus.Message, body []any) ([]any, error) {
	var received dbus.Signature
	if header, ok := msg.Headers[dbus.FieldSignature]; ok {
		received, _ = header.Value().(dbus.Signature)
	}
	if expected := dbus.SignatureOf(m.member.in...); received.String() != expected.String() {
		return nil, atspiInvalid("the call carries arguments %q but the method takes %q; follow the AT-SPI interface XML", received.String(), expected.String())
	}
	return body, nil
}

func (m atspiMethod) Call(args ...any) ([]any, error) {
	if m.text {
		if err := m.object.requireText(); err != nil {
			return nil, err
		}
	}
	return m.member.call(m.object, args)
}

func (m atspiMethod) NumArguments() int { return len(m.member.in) }

func (m atspiMethod) NumReturns() int { return len(m.member.out) }

func (m atspiMethod) ArgumentValue(position int) any { return m.member.in[position] }

func (m atspiMethod) ReturnValue(position int) any { return m.member.out[position] }

func (atspiStale) LookupInterface(string) (dbus.Interface, bool) { return atspiStale{}, true }

func (atspiStale) LookupMethod(string) (dbus.Method, bool) { return atspiStale{}, true }

func (atspiStale) DecodeArguments(_ *dbus.Conn, _ string, _ *dbus.Message, body []any) ([]any, error) {
	return body, nil
}

func (atspiStale) Call(...any) ([]any, error) {
	return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.UnknownObject", Body: []any{ErrElementUnavailable.Error()}}
}

func (atspiStale) NumArguments() int { return 0 }

func (atspiStale) NumReturns() int { return 0 }

func (atspiStale) ArgumentValue(int) any { return nil }

func (atspiStale) ReturnValue(int) any { return nil }

var atspiPropertyMembers = map[string]atspiMember{
	"Get": {in: []any{"", ""}, out: []any{dbus.Variant{}}, call: func(o *atspiObject, args []any) ([]any, error) {
		value, err := o.property(args[0].(string), args[1].(string))
		if err != nil {
			return nil, err
		}
		return []any{dbus.MakeVariant(value)}, nil
	}},
	"GetAll": {in: []any{""}, out: []any{map[string]dbus.Variant{}}, call: atspiGetAll},
	"Set":    {in: []any{"", "", dbus.Variant{}}, call: atspiSet},
}

func (o *atspiObject) supports(iface string) error {
	if slices.Contains(o.interfaces(), iface) {
		return nil
	}
	return dbus.Error{Name: "org.freedesktop.DBus.Error.UnknownInterface", Body: []any{fmt.Sprintf("the object does not implement %s; ask GetInterfaces first", iface)}}
}

func (o *atspiObject) property(iface, name string) (any, error) {
	if err := o.supports(iface); err != nil {
		return nil, err
	}
	getter, ok := atspiInterfaces[iface].properties[name]
	if !ok {
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.UnknownProperty", Body: []any{fmt.Sprintf("%s has no property %s; follow the AT-SPI interface XML", iface, name)}}
	}
	return getter(o)
}

func atspiGetAll(o *atspiObject, args []any) ([]any, error) {
	iface := args[0].(string)
	if err := o.supports(iface); err != nil {
		return nil, err
	}
	values := make(map[string]dbus.Variant)
	for name, getter := range atspiInterfaces[iface].properties {
		value, err := getter(o)
		if err != nil {
			return nil, err
		}
		values[name] = dbus.MakeVariant(value)
	}
	return []any{values}, nil
}

func atspiSet(o *atspiObject, args []any) ([]any, error) {
	iface, name, value := args[0].(string), args[1].(string), args[2].(dbus.Variant)
	if _, err := o.property(iface, name); err != nil {
		return nil, err
	}
	if iface != atspiApplication || name != "Id" {
		return nil, dbus.Error{Name: "org.freedesktop.DBus.Error.PropertyReadOnly", Body: []any{fmt.Sprintf("%s.%s is read-only; only the registry sets org.a11y.atspi.Application.Id", iface, name)}}
	}
	id, ok := value.Value().(int32)
	if !ok {
		return nil, atspiInvalid("Id must be an int32 but the call carries %s; send the value with signature i", value.Signature().String())
	}
	o.bus.id.Store(id)
	return nil, nil
}

func atspiInvalid(format string, args ...any) error {
	return dbus.Error{Name: "org.freedesktop.DBus.Error.InvalidArgs", Body: []any{fmt.Sprintf(format, args...)}}
}
