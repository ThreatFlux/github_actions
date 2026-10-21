"""Guard the input boundary of Docker-based CI checks."""

import argparse
import unittest
from unittest.mock import patch

import check_static_image
import smoke_container_https


class ContainerInputTests(unittest.TestCase):
    def test_image_operands_reject_options_and_shell_syntax(self):
        for module in (check_static_image, smoke_container_https):
            for value in ("", "--privileged", "image;id", "image\n--privileged", "$(id)", "image name"):
                with self.subTest(module=module.__name__, value=value):
                    with self.assertRaises(argparse.ArgumentTypeError):
                        module.image_reference(value)

    def test_registry_ports_tags_and_digests_remain_supported(self):
        for module in (check_static_image, smoke_container_https):
            for value in ("localhost:5000/team/image:scan", "ghcr.io/threatflux/image:scan",
                          "image@sha256:" + "a" * 64):
                with self.subTest(module=module.__name__, value=value):
                    self.assertEqual(module.image_reference(value), value)

    def test_invalid_container_id_is_never_reused_as_a_docker_argument(self):
        with patch("sys.argv", ["check_static_image.py", "image:scan"]), \
             patch.object(check_static_image.subprocess, "check_output", return_value="--help\n") as output:
            with self.assertRaisesRegex(ValueError, "invalid container ID"):
                check_static_image.main()
            self.assertEqual(output.call_count, 1)
            self.assertFalse(output.call_args.kwargs["shell"])

    def test_invalid_image_id_still_removes_container(self):
        container = "a" * 64
        with patch("sys.argv", ["check_static_image.py", "image:scan"]), \
             patch.object(check_static_image.subprocess, "check_output",
                          side_effect=[container, '[{"Image":"--help"}]']) as output, \
             patch.object(check_static_image.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "invalid image ID"):
                check_static_image.main()
            self.assertEqual(output.call_count, 2)
            self.assertEqual(run.call_args.args[0], ["docker", "rm", container])
            self.assertFalse(run.call_args.kwargs["shell"])


if __name__ == "__main__":
    unittest.main()
