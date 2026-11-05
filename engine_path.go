package main

import (
	"os"
	"path/filepath"
	"runtime"
)

// enginePathBesideExecutable resolves the bundled Rust engine on all supported
// desktop platforms. macOS app bundles place both executables under
// <app>.app/Contents/MacOS; Windows uses the .exe suffix.
func enginePathBesideExecutable(executablePath, goos string) string {
	name := "df-engine"
	if goos == "windows" {
		name += ".exe"
	}
	return filepath.Join(filepath.Dir(executablePath), name)
}

func resolveEnginePath() string {
	if configured := os.Getenv("DF_ENGINE_PATH"); configured != "" {
		return configured
	}
	executable, err := os.Executable()
	if err != nil {
		// A descriptive fallback, so the startup error points to the required
		// executable rather than mistakenly claiming the Windows binary exists.
		return enginePathBesideExecutable(".", runtime.GOOS)
	}
	return enginePathBesideExecutable(executable, runtime.GOOS)
}
