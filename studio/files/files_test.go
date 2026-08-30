package files

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWorkspaceListRead(t *testing.T) {
	d := t.TempDir()
	os.WriteFile(filepath.Join(d, "b.YAML"), []byte("b"), 0600)
	os.WriteFile(filepath.Join(d, "a.yaml"), []byte("a"), 0600)
	os.WriteFile(filepath.Join(d, "x.txt"), []byte("x"), 0600)
	w, err := Open(d)
	if err != nil {
		t.Fatal(err)
	}
	fs, err := w.ListDSL()
	if err != nil || len(fs) != 2 || fs[0].Name != "a.yaml" {
		t.Fatalf("files=%v err=%v", fs, err)
	}
	b, err := w.Read(fs[0].Path)
	if err != nil || string(b) != "a" {
		t.Fatalf("read=%q err=%v", b, err)
	}
	if _, err = w.Read(filepath.Join(d, "..", "outside.yaml")); err == nil {
		t.Fatal("expected path rejection")
	}
}

func TestWorkspaceErrors(t *testing.T) {
	if _, err := Open(filepath.Join(t.TempDir(), "missing")); err == nil {
		t.Fatal("expected error")
	}
}
