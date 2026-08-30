import os
import sys
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))

from dfengine import Engine, EngineError, start


class FakeProcess:
    def __init__(self, responses: str = "", return_code: int | None = None, wait_code: int | None = None):
        self.stdin = StringIO()
        self.stdout = StringIO(responses)
        self.return_code = return_code
        self.wait_code = wait_code
        self.killed = False

    def poll(self):
        return self.return_code

    def wait(self, timeout=None):
        if self.wait_code is not None:
            return self.wait_code
        return 0 if self.return_code is None else self.return_code

    def kill(self):
        self.killed = True
        self.return_code = -9


class EngineSdkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        suffix = ".exe" if os.name == "nt" else ""
        cls.engine_path = ROOT / "target" / "debug" / f"df-engine{suffix}"
        cls.project_path = ROOT / "examples" / "two-tank" / "two-tank.dfproject"

    def test_start_get_execute_stop(self):
        engine = start(self.project_path, self.engine_path)
        try:
            metadata = engine.get("project_metadata")
            self.assertEqual(metadata["name"], "two_tank")
            self.assertEqual(metadata["cycle_time"], 0.5)
            initial = engine.execute(0)
            result = engine.execute(3, {"plant_inlet_flow": 0.002})
            self.assertEqual(initial.cycle_index, 0)
            self.assertEqual(result.cycle_index, 3)
            self.assertEqual(result.simulation_time, 1.5)
            self.assertGreater(result.values["tank_1.level"], initial.values["tank_1.level"])
        finally:
            engine.stop()

    def test_invalid_batch_does_not_advance(self):
        with start(self.project_path, self.engine_path) as engine:
            before = engine.execute(0)
            with self.assertRaises(EngineError) as caught:
                engine.execute(1, {"plant_inlet_flow": 0.002, "tank_1.level": 0.9})
            self.assertEqual(caught.exception.code, 1202)
            self.assertEqual(engine.execute(0), before)

    def test_batch_matches_single_cycles(self):
        with start(self.project_path, self.engine_path) as batch:
            batch_result = batch.execute(10)
        with start(self.project_path, self.engine_path) as singles:
            for _ in range(10):
                single_result = singles.execute(1)
        self.assertEqual(batch_result, single_result)

    def test_python_side_argument_and_response_validation(self):
        with start(self.project_path, self.engine_path) as engine:
            for invalid in (-1, 1.5, True):
                with self.assertRaises(ValueError):
                    engine.execute(invalid)
            with patch.object(engine, "_exchange", return_value={}):
                with self.assertRaises(EngineError) as caught:
                    engine.execute(1)
            self.assertEqual(caught.exception.code, 1302)

    def test_environment_discovery_and_idempotent_stop(self):
        fake = FakeProcess(
            '{"id":1,"code":0,"content":{"name":"two_tank"}}\n'
            '{"id":2,"code":0}\n'
        )
        with patch.dict(os.environ, {"DF_ENGINE_PATH": "custom-engine"}), patch(
            "dfengine.client.subprocess.Popen", return_value=fake
        ) as popen:
            engine = start("project.dfproject")
            engine.stop()
            engine.stop()
        self.assertEqual(popen.call_args.args[0][0], "custom-engine")

    def test_start_failure_terminates_child(self):
        fake = FakeProcess()
        with patch("dfengine.client.subprocess.Popen", return_value=fake):
            with self.assertRaises(EngineError) as caught:
                start("bad.dfproject", "engine")
        self.assertEqual(caught.exception.code, 1301)
        self.assertTrue(fake.killed)

    def test_protocol_failure_paths(self):
        stopped = Engine(FakeProcess(return_code=0))
        with self.assertRaises(EngineError):
            stopped.get("project_metadata")
        stopped._stopped = True

        mismatch = Engine(FakeProcess('{"id":99,"code":0,"content":{}}\n'))
        with self.assertRaises(EngineError) as caught:
            mismatch.get("project_metadata")
        self.assertEqual(caught.exception.code, 1302)
        mismatch._terminate()

    def test_nonzero_stop_and_destructor_cleanup(self):
        failed = Engine(FakeProcess('{"id":1,"code":0}\n', wait_code=7))
        with self.assertRaises(EngineError) as caught:
            failed.stop()
        self.assertEqual(caught.exception.code, 1301)

        process = FakeProcess()
        engine = Engine(process)
        engine.__del__()
        self.assertTrue(process.killed)


if __name__ == "__main__":
    unittest.main()
