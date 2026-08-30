package files

import (
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

var ErrInvalidWorkspace = errors.New("workspace must be an existing directory")

type File struct {
	Path string `json:"path"`
	Name string `json:"name"`
}

type Workspace struct{ root string }

func Open(root string) (Workspace, error) {
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return Workspace{}, ErrInvalidWorkspace
	}
	abs, err := filepath.Abs(root)
	if err != nil {
		return Workspace{}, err
	}
	return Workspace{root: abs}, nil
}

func (w Workspace) Root() string { return w.root }

func (w Workspace) ListDSL() ([]File, error) {
	entries, err := os.ReadDir(w.root)
	if err != nil {
		return nil, err
	}
	result := make([]File, 0)
	for _, entry := range entries {
		if entry.IsDir() || !strings.EqualFold(filepath.Ext(entry.Name()), ".yaml") {
			continue
		}
		result = append(result, File{Path: filepath.Join(w.root, entry.Name()), Name: entry.Name()})
	}
	sort.Slice(result, func(i, j int) bool { return result[i].Name < result[j].Name })
	return result, nil
}

func (w Workspace) Read(path string) ([]byte, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	rel, err := filepath.Rel(w.root, abs)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return nil, os.ErrPermission
	}
	return os.ReadFile(abs)
}
