#!/usr/bin/env python3
"""Unit tests for tb_runner.py (no Docker needed)."""

import dataclasses
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from tb_runner import Task, TaskResult, discover_tasks, write_report


def make_repo(tasks: dict) -> Path:
    """Build a fake terminal-bench-2 checkout."""
    d = Path(tempfile.mkdtemp())
    for name, cfg in tasks.items():
        td = d / name
        td.mkdir()
        (td / "task.toml").write_text(
            f"[environment]\ndocker_image = \"img/{name}:latest\"\n"
            f"[agent]\ntimeout_sec = {cfg.get('timeout', 900)}\n"
            f"[metadata]\ndifficulty = \"{cfg.get('difficulty', 'easy')}\"\n"
            f"category = \"{cfg.get('category', 'test')}\"\n"
        )
        (td / "instruction.md").write_text(f"Do {name}.")
    return d


class TestDiscover(unittest.TestCase):
    def test_discovers_tasks(self):
        repo = make_repo({"alpha": {}, "beta": {"difficulty": "hard"}})
        tasks = discover_tasks(repo)
        self.assertEqual(set(tasks), {"alpha", "beta"})
        self.assertEqual(tasks["alpha"].image, "img/alpha:latest")
        self.assertEqual(tasks["alpha"].instruction, "Do alpha.")
        self.assertEqual(tasks["beta"].difficulty, "hard")
        self.assertEqual(tasks["beta"].agent_timeout_sec, 900)

    def test_skips_dirs_without_task_toml(self):
        repo = make_repo({"alpha": {}})
        (repo / "notask").mkdir()
        (repo / "notask" / "instruction.md").write_text("x")
        self.assertEqual(set(discover_tasks(repo)), {"alpha"})

    def test_empty_repo(self):
        self.assertEqual(discover_tasks(Path(tempfile.mkdtemp())), {})


class TestReport(unittest.TestCase):
    def test_report_table(self):
        results = [
            TaskResult("a", True, 61.0, 0, None),
            TaskResult("b", False, 120.5, 1, None),
            TaskResult("c", None, 5.0, None, "docker pull failed: boom"),
        ]
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "report.md"
            write_report(p, results, "qwen3:4b", "http://x/v1", "zen 0.1")
            text = p.read_text()
        self.assertIn("1/3", text)
        self.assertIn("| a | PASS | 61s |", text)
        self.assertIn("| b | FAIL | 120s |", text)  # 120.5 formats as 120s
        self.assertIn("| c | ERROR | 5s | docker pull failed: boom |", text)
        self.assertIn("## Methodology", text)
        self.assertIn("## Limitations", text)
        self.assertIn("qwen3:4b", text)

    def test_results_json_roundtrip(self):
        results = [TaskResult("a", True, 10.0, 0, None)]
        data = json.loads(json.dumps([dataclasses.asdict(r) for r in results]))
        self.assertEqual(data[0]["passed"], True)
        self.assertEqual(data[0]["name"], "a")


if __name__ == "__main__":
    unittest.main()
