//go:build !windows

package childprocess

import "os/exec"

func HideWindow(*exec.Cmd) {}
