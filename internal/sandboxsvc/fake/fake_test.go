package fake

import (
	"io"
	"net"
	"testing"

	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

func TestCountersAndConnect(t *testing.T) {
	service := New(func(conn net.Conn) {
		_, _ = io.Copy(conn, conn)
	})
	ctx := t.Context()
	id, err := service.Create(ctx, sandboxsvc.Spec{Name: "thread-a"})
	if err != nil {
		t.Fatal(err)
	}
	if service.CreateCount() != 1 {
		t.Fatalf("create count %d", service.CreateCount())
	}
	conn, err := service.Connect(ctx, id, sandboxsvc.ExecServer)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Write([]byte("ok")); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 2)
	if _, err := io.ReadFull(conn, buf); err != nil {
		t.Fatal(err)
	}
	if string(buf) != "ok" {
		t.Fatalf("echo %q", buf)
	}
	_ = conn.Close()
	if err := service.Destroy(ctx, id); err != nil {
		t.Fatal(err)
	}
	if service.DestroyCount() != 1 {
		t.Fatalf("destroy count %d", service.DestroyCount())
	}
}
