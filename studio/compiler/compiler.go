package compiler

import (
	"context"
	"fmt"
	"os"
	"os/exec"

	"datafactory-next/studio/internal/childprocess"
)

type Result struct {
	Artifact string `json:"artifact"`
	Output   string `json:"output"`
}
type Runner func(context.Context, string, ...string) ([]byte, error)
type Compiler struct {
	EnginePath string
	run        Runner
}

func New(enginePath string) Compiler { return Compiler{EnginePath: enginePath, run: defaultRunner} }
func NewWithRunner(enginePath string, run Runner) Compiler {
	return Compiler{EnginePath: enginePath, run: run}
}

func defaultRunner(ctx context.Context, path string, args ...string) ([]byte, error) {
	command := exec.CommandContext(ctx, path, args...)
	childprocess.HideWindow(command)
	return command.CombinedOutput()
}

func (c Compiler) Compile(ctx context.Context, source, artifact string) (Result, error) {
	if source == "" || artifact == "" {
		return Result{}, fmt.Errorf("source and artifact are required")
	}
	if _, err := os.Stat(source); err != nil {
		return Result{}, fmt.Errorf("source: %w", err)
	}
	if c.EnginePath == "" {
		return Result{}, fmt.Errorf("engine path is required")
	}
	out, err := c.run(ctx, c.EnginePath, "compile", source, artifact)
	if err != nil {
		return Result{Output: string(out)}, fmt.Errorf("compile failed: %w", err)
	}
	return Result{Artifact: artifact, Output: string(out)}, nil
}
