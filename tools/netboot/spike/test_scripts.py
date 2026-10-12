"""The iPXE script templates (no network, no QEMU)."""

import unittest

import scripts

REG = "http://10.0.2.2:8420"


class MenuTests(unittest.TestCase):
    def test_menu_shape(self):
        text = scripts.boot_menu(REG, "spike build")
        lines = text.splitlines()
        self.assertEqual(lines[0], "#!ipxe")
        self.assertIn(f"set reg {REG}", lines)
        self.assertIn("item b_spike spike build", lines)
        self.assertIn("item shell  iPXE shell", lines)
        self.assertTrue(any(l.startswith("item local") for l in lines))
        self.assertIn("choose --default b_spike --timeout 5000 target || goto local", lines)
        self.assertIn("goto ${target}", lines)

    def test_every_goto_has_a_label(self):
        text = scripts.boot_menu(REG, "x")
        labels = {l[1:] for l in text.splitlines() if l.startswith(":")}
        for line in text.splitlines():
            for word in line.split("goto ")[1:]:
                target = word.split()[0]
                if target != "${target}":
                    self.assertIn(target, labels, line)

    def test_every_menu_item_is_handled(self):
        text = scripts.boot_menu(REG, "x")
        labels = {l[1:] for l in text.splitlines() if l.startswith(":")}
        items = [l.split()[1] for l in text.splitlines() if l.startswith("item ") and l.split()[1] != "--gap"]
        self.assertEqual(sorted(items), sorted(labels - {"menu"}))

    def test_timeout_is_a_parameter(self):
        self.assertIn("--timeout 12000", scripts.boot_menu(REG, "x", 12000))


class BuildTests(unittest.TestCase):
    def test_three_imgfetch_then_chain(self):
        lines = scripts.build_script(REG).splitlines()
        self.assertEqual(lines, [
            "#!ipxe",
            f"imgfetch --name kernel-x86_64 {REG}/files/kernel-x86_64",
            f"imgfetch --name boot.json {REG}/files/boot.json",
            f"imgfetch --name ramdisk {REG}/files/ramdisk",
            f"chain --name bootx64.efi {REG}/files/bootx64.efi",
        ])

    def test_urls_are_absolute(self):
        self.assertNotIn("${", scripts.build_script(REG))

    def test_sanboot(self):
        self.assertEqual(scripts.sanboot_script(REG).splitlines(),
                         ["#!ipxe", f"sanboot --no-describe {REG}/files/esp.img"])


class AutoexecTests(unittest.TestCase):
    def test_exit_variant_is_the_plans(self):
        text = scripts.autoexec("http://192.168.1.5:8420", "exit")
        self.assertIn("dhcp || goto fail", text)
        self.assertIn("chain http://192.168.1.5:8420/boot.ipxe?mac=${net0/mac} || goto fail", text)
        self.assertTrue(text.rstrip().endswith("exit"))
        self.assertIn("sleep 5", text)

    def test_shell_variant_stays_on_screen(self):
        text = scripts.autoexec(REG, "shell")
        self.assertTrue(text.rstrip().endswith("shell"))
        self.assertNotIn("exit", text)

    def test_bad_variant_refused(self):
        with self.assertRaises(ValueError):
            scripts.autoexec(REG, "reboot")

    def test_registry_url_drops_port_80(self):
        self.assertEqual(scripts.registry_url("h", 80), "http://h")
        self.assertEqual(scripts.registry_url("h", 8420), "http://h:8420")


if __name__ == "__main__":
    unittest.main()
