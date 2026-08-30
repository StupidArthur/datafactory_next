//go:build windows

package childprocess

import (
	"os/exec"
	"syscall"
)

func HideWindow(command *exec.Cmd) {
	command.SysProcAttr = &syscall.SysProcAttr{HideWindow: true}
}
