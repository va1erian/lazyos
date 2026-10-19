#!/usr/bin/env python3
"""Launcher tests for the LazyAgent AI agent (LAZYOS_AGENT=1,
docs/agent.md) from the Simple tab, the Advanced tab and run_demo.

Run: python tools/lazygui/test_catalog_agent.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class AgentTests(unittest.TestCase):
    """The LazyAgent AI agent (LAZYOS_AGENT=1, docs/agent.md) from the
    Simple tab, the Advanced tab and run_demo: a desktop app that brings the
    network stack (and not the HTTPS tools: the CA bundle is in every image)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_brings_the_desktop_and_the_stack(self) -> None:
        env = catalog.build_env({**self.base(), "agent": True})
        for name in ("LAZYOS_AGENT", "LAZYOS_DESKTOP", "LAZYOS_NETD"):
            self.assertEqual(env.get(name), "1", name)
        self.assertNotIn("LAZYOS_TLS", env)
        off = catalog.build_env({**self.base(), "desktop": True})
        self.assertNotIn("LAZYOS_AGENT", off)

    def test_simple_desktop_offers_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", agent=True)
        self.assertTrue(cfg["agent"])
        self.assertTrue(cfg["net"])
        self.assertFalse(cfg["tls"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", agent=True)
        self.assertFalse(cli["agent"])
        self.assertFalse(cli["net"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["agent"])

    def test_sessions_get_the_card_with_agent_alone(self) -> None:
        self.assertIn("--net", catalog.net_flags({"net": False, "tls": False, "agent": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--agent", demo_argv(agent=True, skip_build=False))
        self.assertNotIn("--agent", demo_argv(skip_build=False))
        self.assertNotIn("--agent", demo_argv(agent=True, skip_build=True))
        self.assertIn("--net", demo_argv(agent=True, skip_build=False))

    def test_simple_start_plans_the_client(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", agent=True)
        plan = catalog.build_plan(cfg)
        self.assertIn("--agent", plan[-1]["argv"])

    def test_run_demo_flag_implies_the_desktop_and_the_network(self) -> None:
        import sys
        from pathlib import Path
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from demo_args import make_parser, parse_args
        parser = make_parser("test", Path("target/lazyos.img"))
        args = parse_args(parser, ["--agent"])[0]
        self.assertTrue(args.desktop and args.net)
        self.assertFalse(args.tls)


if __name__ == "__main__":
    unittest.main()
