"""Local wrapper tests with fake Docker/downloads, not a test of BuildKit compatibility."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("compare_buildx.sh").resolve()


class CompareBuildxTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.config = self.root / "docker"
        self.plugin = self.config / "cli-plugins" / "docker-buildx"
        self.plugin.parent.mkdir(parents=True)
        self.original = b"#!/bin/bash\necho 'github.com/docker/buildx v0.8.0 baseline'\n"
        self.plugin.write_bytes(self.original)
        self.plugin.chmod(0o755)
        # The wrapper must not replace or copy Docker credentials/builder state.
        (self.config / "config.json").write_text("credential sentinel")
        (self.config / "buildx").mkdir()
        (self.config / "buildx" / "sentinel").write_text("builder sentinel")
        self.command("docker", """
case "$*" in
    --version) echo 'Docker version 20.10.17' ;;
    'buildx version')
        if [ "${WRONG_CLIENT:-0}" = 1 ]; then
            echo 'github.com/docker/buildx v0.9.0 unexpected'
        elif [ "${IGNORE_UPGRADE:-0}" = 1 ]; then
            echo 'github.com/docker/buildx v0.8.0 baseline'
        else
            "$DOCKER_CONFIG/cli-plugins/docker-buildx" version
        fi ;;
    'buildx inspect ci')
        test "$BUILDX_BUILDER" = ci
        echo "Driver: ${DRIVER:-kubernetes}" ;;
    *) echo "Unexpected Docker command: $*" >&2; exit 99 ;;
esac
""")
        self.command("uname", 'echo "${TEST_ARCH:-x86_64}"')
        self.command("curl", """
printf '%s\n' "$*" >download.txt
if [ "${DOWNLOAD_FAIL:-0}" = 1 ]; then exit 22; fi
# The output path and URL are the final two arguments.
printf '#!/bin/bash\\necho "github.com/docker/buildx v0.36.0 treatment"\\n' >"${@: -2:1}"
""")
        self.command("apt-get", """
printf '%s\n' "$*" >>packages.txt
if [ "${INSTALL_FAIL:-0}" = 1 ]; then exit 100; fi
if [ "$1" = install ]; then touch curl-installed; fi
""")
        self.bash_env = self.root / "bash-env"
        self.bash_env.write_text("""
# Simulate a minimal CI image even if the local host has curl.
command() {
    if [ "${MISSING_CURL:-0}" = 1 ] && [ "$*" = '-v curl' ] && [ ! -f curl-installed ]; then
        return 1
    fi
    builtin command "$@"
}
""")
        self.command("sha256sum", """
# Successful verification is mocked. The failure case uses real sha256sum.
if [ "${CHECKSUM_FAIL:-0}" = 1 ]; then exec /usr/bin/sha256sum "$@"; fi
read -r checksum file
case "${TEST_ARCH:-x86_64}" in
    x86_64) expected=07823fdfcd82a41be90155a8b16876c1a780a6462de805a9f3f63b3119ccfb99 ;;
    aarch64) expected=70382de03915c07c488ae4ddc4f7e169ee978f953e754ecfce110ba017e0132b ;;
esac
test "$checksum" = "$expected" && test -f "$file"
""")
        self.compile = self.root / "compile"
        self.compile.write_text("""#!/bin/bash
set -eu
test "$BUILDX_BUILDER" = ci
touch compile-ran
docker buildx version
if [ "${SLOW_COMPILE:-0}" = 1 ]; then sleep 10; fi
exit "${COMPILE_RC:-0}"
""")
        self.compile.chmod(0o755)

    def command(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/bash\nset -eu\n" + body + "\n")
        path.chmod(0o755)

    def run_spike(self, arm="current", **overrides):
        env = os.environ.copy()
        env.update(
            PATH=str(self.bin) + os.pathsep + env["PATH"],
            DOCKER_CONFIG=str(self.config),
            BUILDX_ARM=arm,
            SPIKE_COMPILE_SCRIPT=str(self.compile),
            SPIKE_BUILD_LIMIT="5s",
            BASH_ENV=str(self.bash_env),
        )
        env.update(overrides)
        run = subprocess.run(
            ["bash", str(SCRIPT)], cwd=self.root, env=env,
            capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(self.plugin.read_bytes(), self.original, run.stdout)
        self.assertEqual((self.config / "config.json").read_text(), "credential sentinel")
        self.assertEqual((self.config / "buildx" / "sentinel").read_text(), "builder sentinel")
        result = (self.root / "result.txt").read_text()
        return run, result

    def test_current_success(self):
        run, result = self.run_spike()
        self.assertEqual(run.returncode, 0, run.stdout)
        self.assertIn("outcome=success", result)
        self.assertIn("v0.8.0", (self.root / "build.log").read_text())
        self.assertFalse((self.root / "download.txt").exists())

    def test_upgraded_success_both_client_architectures(self):
        for host_arch, asset_arch in [("x86_64", "amd64"), ("aarch64", "arm64")]:
            with self.subTest(architecture=host_arch):
                run, result = self.run_spike("upgraded", TEST_ARCH=host_arch)
                self.assertEqual(run.returncode, 0, run.stdout)
                self.assertIn("outcome=success", result)
                self.assertIn("v0.36.0", (self.root / "build.log").read_text())
                self.assertIn(f"buildx-v0.36.0.linux-{asset_arch}",
                              (self.root / "download.txt").read_text())

    def test_compile_failure_is_preserved_and_plugin_restored(self):
        run, result = self.run_spike("upgraded", COMPILE_RC="42")
        self.assertEqual(run.returncode, 42, run.stdout)
        self.assertIn("outcome=compile_failure", result)

    def test_compile_bound_is_a_failure_with_artifacts(self):
        run, result = self.run_spike("upgraded", SLOW_COMPILE="1", SPIKE_BUILD_LIMIT="1s")
        self.assertEqual(run.returncode, 124, run.stdout)
        self.assertIn("outcome=compile_bound_hit", result)
        self.assertTrue((self.root / "buildx-spike.log").exists())
        self.assertTrue((self.root / "build.log").exists())

    def test_download_failure_does_not_compile(self):
        run, result = self.run_spike("upgraded", DOWNLOAD_FAIL="1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_missing_curl_is_installed_before_download(self):
        run, result = self.run_spike("upgraded", MISSING_CURL="1")
        self.assertEqual(run.returncode, 0, run.stdout)
        self.assertIn("outcome=success", result)
        self.assertEqual((self.root / "packages.txt").read_text().splitlines(),
                         ["update -qq", "install -y --no-install-recommends curl ca-certificates"])
        self.assertTrue((self.root / "curl-installed").exists())

    def test_existing_curl_requires_no_package_install(self):
        run, _ = self.run_spike("upgraded")
        self.assertEqual(run.returncode, 0, run.stdout)
        self.assertFalse((self.root / "packages.txt").exists())

    def test_package_install_failure_does_not_download_or_compile(self):
        run, result = self.run_spike("upgraded", MISSING_CURL="1", INSTALL_FAIL="1")
        self.assertEqual(run.returncode, 100, run.stdout)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "download.txt").exists())
        self.assertFalse((self.root / "compile-ran").exists())

    def test_checksum_failure_does_not_install_or_compile(self):
        run, result = self.run_spike("upgraded", CHECKSUM_FAIL="1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_unexpected_fleet_client_is_rejected(self):
        run, result = self.run_spike(WRONG_CLIENT="1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_unselected_upgrade_is_rejected_and_plugin_restored(self):
        run, result = self.run_spike("upgraded", IGNORE_UPGRADE="1")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_wrong_driver_is_rejected_and_plugin_restored(self):
        run, result = self.run_spike("upgraded", DRIVER="docker-container")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_invalid_arm_is_rejected(self):
        run, result = self.run_spike("invalid")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())

    def test_unsupported_architecture_is_rejected(self):
        run, result = self.run_spike("upgraded", TEST_ARCH="riscv64")
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("outcome=setup_failure", result)
        self.assertFalse((self.root / "compile-ran").exists())


if __name__ == "__main__":
    unittest.main()
