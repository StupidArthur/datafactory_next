package compiler

import (
	"context"
	"os"
	"testing"
)

func TestCompile(t *testing.T) {
	s := t.TempDir() + "/x.yaml"
	os.WriteFile(s, []byte("version: 2"), 0600)
	c := NewWithRunner("engine", func(_ context.Context, path string, args ...string) ([]byte, error) {
		if path != "engine" || len(args) != 3 || args[0] != "compile" {
			t.Fatal(path, args)
		}
		return []byte("ok"), nil
	})
	r, err := c.Compile(context.Background(), s, t.TempDir()+"/x.dfproject")
	if err != nil || r.Output != "ok" {
		t.Fatal(r, err)
	}
}
func TestCompileErrors(t *testing.T) {
	c := New("")
	if _, err := c.Compile(context.Background(), "", "x"); err == nil {
		t.Fatal("required")
	}
	c = New("engine")
	if _, err := c.Compile(context.Background(), "missing", "x"); err == nil {
		t.Fatal("missing")
	}
	s := t.TempDir() + "/x.yaml"
	os.WriteFile(s, []byte("x"), 0600)
	c = New("")
	if _, err := c.Compile(context.Background(), s, "x"); err == nil {
		t.Fatal("engine")
	}
	c = NewWithRunner("engine", func(context.Context, string, ...string) ([]byte, error) { return []byte("bad"), os.ErrPermission })
	if _, err := c.Compile(context.Background(), s, "x"); err == nil {
		t.Fatal("runner")
	}
}
