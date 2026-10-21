"""Guard the input boundary of Docker-based CI checks."""

import argparse
import unittest
from unittest.mock import patch

import check_static_image
import smoke_container_https


class ContainerInputTests(unittest.TestCase):
    """Keep container-check parsing and cleanup independent of a Docker daemon."""

    def test_image_operands_reject_options_and_shell_syntax(self):
        """Reject image operands that could be mistaken for options or shell syntax."""
        for module in (check_static_image, smoke_container_https):
            for value in ("", "--privileged", "image;id", "image\n--privileged", "$(id)", "image name"):
                with self.subTest(module=module.__name__, value=value):
                    with self.assertRaises(argparse.ArgumentTypeError):
                        module.image_reference(value)

    def test_registry_ports_tags_and_digests_remain_supported(self):
        """Preserve ordinary registry, tag and digest image references."""
        for module in (check_static_image, smoke_container_https):
            for value in ("localhost:5000/team/image:scan", "ghcr.io/threatflux/image:scan",
                          "image@sha256:" + "a" * 64):
                with self.subTest(module=module.__name__, value=value):
                    self.assertEqual(module.image_reference(value), value)

    def test_invalid_container_id_is_never_reused_as_a_docker_argument(self):
        """Stop before inspecting an invalid Docker create response."""
        with patch("sys.argv", ["check_static_image.py", "image:scan"]), \
             patch.object(check_static_image.shutil, "which", return_value="/fixture/bin/docker"), \
             patch.object(check_static_image.subprocess, "check_output", return_value="--help\n") as output:
            with self.assertRaisesRegex(ValueError, "invalid container ID"):
                check_static_image.main()
            self.assertEqual(output.call_count, 1)
            self.assertFalse(output.call_args.kwargs["shell"])

    def test_invalid_image_id_still_removes_container(self):
        """Remove the created container when inspection returns an invalid image ID."""
        container = "a" * 64
        with patch("sys.argv", ["check_static_image.py", "image:scan"]), \
             patch.object(check_static_image.shutil, "which", return_value="/fixture/bin/docker"), \
             patch.object(check_static_image.subprocess, "check_output",
                          side_effect=[container, '[{"Image":"--help"}]']) as output, \
             patch.object(check_static_image.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "invalid image ID"):
                check_static_image.main()
            self.assertEqual(output.call_count, 2)
            self.assertEqual(run.call_args.args[0], ["docker", "rm", container])
            self.assertFalse(run.call_args.kwargs["shell"])

    def test_missing_docker_fails_before_starting_a_process(self):
        """Report a missing Docker executable without invoking a subprocess."""
        with patch("sys.argv", ["check_static_image.py", "image:scan"]), \
             patch.object(check_static_image.shutil, "which", return_value=None), \
             patch.object(check_static_image.subprocess, "check_output") as output:
            with self.assertRaisesRegex(RuntimeError, "Docker is required"):
                check_static_image.main()
            output.assert_not_called()

    def test_ca_provenance_requires_installed_package_and_version(self):
        """Reject incomplete CA supplier metadata independently of build version policy."""
        status = "Package: ca-certificates\nStatus: install ok installed\nVersion: 20250419+deb13u1\n"
        check_static_image.check_ca_package(status)
        for record in (status.replace("Package: ca-certificates", "Package: another-package"),
                       status.replace("Status: install ok installed", "Status: deinstall ok config-files"),
                       status.replace("Version: 20250419+deb13u1\n", ""),
                       status.replace("20250419+deb13u1", " ")):
            with self.subTest(record=record), self.assertRaises(ValueError):
                check_static_image.check_ca_package(record)


if __name__ == "__main__":
    unittest.main()
