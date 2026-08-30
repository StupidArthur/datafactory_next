from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping


class EngineError(RuntimeError):
    def __init__(self, code: int, message: str):
        super().__init__(f"engine error {code}: {message}")
        self.code = code
        self.message = message


@dataclass(frozen=True)
class Snapshot:
    cycle_index: int
    simulation_time: float
    values: dict[str, Any]


class Engine:
    def __init__(self, process: subprocess.Popen[str]):
        self._process = process
        self._next_id = 1
        self._stopped = False

    @classmethod
    def start(cls, project_path: os.PathLike[str] | str, engine_path: os.PathLike[str] | str | None = None) -> "Engine":
        executable = str(engine_path or os.environ.get("DF_ENGINE_PATH", "df-engine"))
        process = subprocess.Popen(
            [executable, "run", str(Path(project_path))],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        engine = cls(process)
        try:
            engine.get("project_metadata")
        except Exception:
            engine._terminate()
            raise
        return engine

    def execute(self, cycles: int, writes: Mapping[str, Any] | None = None) -> Snapshot:
        if not isinstance(cycles, int) or isinstance(cycles, bool) or cycles < 0:
            raise ValueError("cycles must be a non-negative integer")
        response = self._exchange({"operation": "execute", "cycles": cycles, "writes": dict(writes or {})})
        raw = response.get("snapshot")
        if raw is None:
            raise EngineError(1302, "execute response has no snapshot")
        return Snapshot(int(raw["cycle_index"]), float(raw["simulation_time"]), dict(raw["values"]))

    def get(self, name: str) -> Any:
        return self._exchange({"operation": "get", "name": name}).get("content")

    def stop(self) -> None:
        if self._stopped:
            return
        self._exchange({"operation": "stop"})
        return_code = self._process.wait(timeout=5)
        self._stopped = True
        self._close_streams()
        if return_code != 0:
            raise EngineError(1301, f"engine exited with code {return_code}")

    def __enter__(self) -> "Engine":
        return self

    def __exit__(self, *_: object) -> None:
        self.stop()

    def _exchange(self, request: dict[str, Any]) -> dict[str, Any]:
        if self._stopped or self._process.poll() is not None:
            raise EngineError(1301, "engine process is not running")
        request_id = self._next_id
        self._next_id += 1
        request["id"] = request_id
        assert self._process.stdin is not None and self._process.stdout is not None
        self._process.stdin.write(json.dumps(request, ensure_ascii=False, separators=(",", ":")) + "\n")
        self._process.stdin.flush()
        line = self._process.stdout.readline()
        if not line:
            raise EngineError(1301, "engine closed the protocol stream")
        response = json.loads(line)
        if response.get("id") != request_id:
            raise EngineError(1302, "response id mismatch")
        if response.get("code") != 0:
            raise EngineError(int(response.get("code", 1302)), str(response.get("message", "unknown error")))
        return response

    def _terminate(self) -> None:
        if self._process.poll() is None:
            self._process.kill()
        self._process.wait()
        self._stopped = True
        self._close_streams()

    def _close_streams(self) -> None:
        if self._process.stdin is not None:
            self._process.stdin.close()
        if self._process.stdout is not None:
            self._process.stdout.close()

    def __del__(self) -> None:
        if not getattr(self, "_stopped", True):
            self._terminate()


def start(project_path: os.PathLike[str] | str, engine_path: os.PathLike[str] | str | None = None) -> Engine:
    return Engine.start(project_path, engine_path)
