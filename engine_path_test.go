package main

import (
	"path/filepath"
	"testing"
)

func TestEnginePathBesideExecutable(t *testing.T) {
	for _, tc := range []struct {
		name, goos, executable, expected string
	}{
		{
			name:       "macOS application bundle",
			goos:       "darwin",
			executable: filepath.Join("build", "bin", "datafactory-studio.app", "Contents", "MacOS", "datafactory-studio"),
			expected:   filepath.Join("build", "bin", "datafactory-studio.app", "Contents", "MacOS", "df-engine"),
		},
		{
			name:       "Windows build",
			goos:       "windows",
			executable: filepath.Join("build", "bin", "datafactory-studio.exe"),
			expected:   filepath.Join("build", "bin", "df-engine.exe"),
		},
		{
			name:       "Linux build",
			goos:       "linux",
			executable: filepath.Join("build", "bin", "datafactory-studio"),
			expected:   filepath.Join("build", "bin", "df-engine"),
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := enginePathBesideExecutable(tc.executable, tc.goos); got != tc.expected {
				t.Fatalf("got %q, want %q", got, tc.expected)
			}
		})
	}
}

func TestResolveEnginePathOverride(t *testing.T) {
	desired := filepath.Join("custom", "my-df-engine")
	t.Setenv("DF_ENGINE_PATH", desired)
	if got := resolveEnginePath(); got != desired {
		t.Fatalf("override got %q, want %q", got, desired)
	}
}
