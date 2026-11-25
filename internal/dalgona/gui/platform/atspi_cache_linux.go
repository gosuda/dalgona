//go:build linux

package platform

type atspiCacheItem struct {
	Object      atspiRef
	Application atspiRef
	Parent      atspiRef
	Index       int32
	ChildCount  int32
	Interfaces  []string
	Name        string
	Role        uint32
	Description string
	States      []uint32
}

func (o *atspiObject) cacheItem() atspiCacheItem {
	return atspiCacheItem{
		Object:      o.self(),
		Application: o.bus.ref(atspiRootPath),
		Parent:      o.parent(),
		Index:       o.indexInParent(),
		ChildCount:  clampInt32(len(o.children())),
		Interfaces:  o.interfaces(),
		Name:        o.name(),
		Role:        o.role().value,
		Description: o.description(),
		States:      o.states().words(),
	}
}

func atspiCacheItems(o *atspiObject) any {
	items := []atspiCacheItem{
		o.object(atspiApplicationKind, SemanticNode{}).cacheItem(),
		o.object(atspiFrameKind, SemanticNode{}).cacheItem(),
	}
	atspiWalk(o.snap, func(n SemanticNode) {
		items = append(items, o.object(atspiNodeKind, n).cacheItem())
	})
	return items
}

var atspiCacheMembers = map[string]atspiMember{
	"GetItems": {out: []any{[]atspiCacheItem{}}, call: atspiAnswer(atspiCacheItems)},
}

func (bus *atspiBus) emitCache(member string, body ...any) {
	_ = bus.conn.Emit(atspiCachePath, atspiCache+"."+member, body...)
}
