import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def load_script(name):
    path = Path(__file__).resolve().parents[1] / name
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


prepare = load_script("prepare-publish")
publish = load_script("publish-gpui")


class PublishTests(unittest.TestCase):
    def test_wgpu_target_dependencies_keep_the_original_library_names(self):
        text = '''[package]
name = "wgpu"
version = "30.0.0"
readme = "../README.md"
[lib]
name = "wgpu"
[target.'cfg(target_family = "wasm")'.dependencies.wgpu-core]
version = "30.0.0"
path = "../wgpu-core"
features = ["webgl"]
optional = true
[dependencies.log]
version = "0.4"
[dev-dependencies.naga-test]
path = "../naga-test"
'''
        packages = {name: {"version": "30.0.0"} for name in ("wgpu", "wgpu-core")}
        result = prepare.rewrite_wgpu(text, "wgpu", packages, "0.3.25+zed.e4a373")
        self.assertIn('[lib]\nname = "wgpu"', result)
        self.assertIn('package = "bezel-wgpu-core"', result)
        self.assertIn('version = "=0.3.25+zed.e4a373"', result)
        self.assertIn('features = ["webgl"]\noptional = true', result)
        self.assertIn('[dependencies.log]\nversion = "0.4"', result)
        self.assertNotIn("naga-test", result)

    def test_wgpu_packages_and_dependencies_follow_each_fork_release(self):
        text = '[package]\nname = "wgpu"\nversion = "30.0.0"\n[dependencies.naga]\nversion = "30.0.0"\npath = "../naga"\n'
        root = Path("/release")
        manifests = {name: root / "vendor/wgpu" / name / "Cargo.toml"
                     for name in ("wgpu", "naga")}
        workspace = '[workspace]\nmembers = [\n]\n[workspace.dependencies]\nwgpu = { git = "https://github.com/gfx-rs/wgpu", rev = "abc", default-features = false }\n'
        for version in ("0.3.25+zed.e4a373", "0.3.26+zed.e4a373"):
            with self.subTest(version=version):
                result = prepare.rewrite_wgpu(text, "wgpu", manifests, version)
                self.assertIn(f'name = "bezel-wgpu"\nversion = "{version}"', result)
                self.assertIn(f'package = "bezel-naga"\nversion = "={version}"', result)
                self.assertNotIn("30.0.0", result)
                result = prepare.attach_wgpu(workspace, root, manifests, version)
                self.assertIn(f'package = "bezel-wgpu", version = "={version}"', result)
                self.assertIn('default-features = false', result)

    def test_dependency_outside_the_wgpu_closure_fails(self):
        with self.assertRaisesRegex(SystemExit, "escapes WGPU closure"):
            prepare.rewrite_wgpu(
                '[dependencies.other]\npath = "../other"\n', "wgpu",
                {"wgpu": {"version": "30.0.0"}}, "0.3.25+zed.e4a373")

    def test_registry_patches_are_not_versionless_workspace_dependencies(self):
        text = '''[workspace.dependencies]
async-task = "4.7"
wgpu = { git = "https://github.com/gfx-rs/wgpu", rev = "abc" }
[patch.crates-io]
async-task = { git = "https://github.com/smol-rs/async-task", rev = "abc" }
'''
        self.assertEqual(prepare.versionless_git_deps(text), {"wgpu"})

    def test_mandatory_git_dependency_is_rejected_before_preparation(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[dependencies]\nunknown.workspace = true\n')
            self.assertEqual(prepare.unpublished_dependencies(
                {"gpui": manifest}, {"unknown"}), ["gpui: unknown"])

    def test_multiline_versioned_git_dependency_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('''[dependencies]
xim = { git = "https://github.com/zed-industries/xim-rs.git", features = [
    "x11rb-client",
], package = "zed-xim", version = "0.4.0-zed", optional = true }
''')
            self.assertEqual(prepare.unpublished_dependencies({"gpui_linux": manifest}, set()), [])

    def test_failed_verification_never_uploads(self):
        metadata = {"metadata": {"conta": {"ignore": ["editor"]}},
                    "workspace_members": ["gpui", "wgpu", "editor"],
                    "packages": [{"id": name, "name": package} for name, package in (
                        ("gpui", "bezel-gpui"), ("wgpu", "bezel-wgpu"), ("editor", "editor"))]}
        with patch.object(publish.subprocess, "run") as run, patch(
                "sys.argv", ["publish-gpui", "--publish"]):
            run.side_effect = [subprocess.CompletedProcess([], 0, json.dumps(metadata)),
                               subprocess.CalledProcessError(101, ["cargo", "publish", "--dry-run"])]
            with self.assertRaises(subprocess.CalledProcessError):
                publish.main()
        self.assertEqual(run.call_count, 2)
        command = run.call_args.args[0]
        self.assertIn("--dry-run", command)
        self.assertIn("bezel-wgpu", command)
        self.assertNotIn("editor", command)


class WgpuVendoringTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        upstream = self.root / "upstream"
        upstream.mkdir()
        (upstream / "README.md").write_text("WGPU upstream\n")
        self.packages = {"wgpu": {
            "version": "30.0.0",
            "source": "git+https://github.com/gfx-rs/wgpu?rev=abc#abc",
            "manifest_path": str(upstream / "wgpu" / "Cargo.toml"),
        }}
        self.vendor = self.root / "vendor" / "wgpu"
        self.vendor.mkdir(parents=True)
        (self.vendor / "previous-release").write_text("preserve until success")

    def cargo_vendor(self, command, **kwargs):
        package = Path(command[-1]) / "wgpu-30.0.0"
        package.mkdir(parents=True)
        (package / "Cargo.toml").write_text(
            '[package]\nname = "wgpu"\nversion = "30.0.0"\n')
        (package / ".cargo-checksum.json").write_text("{}")
        return subprocess.CompletedProcess(command, 0)

    def test_temporary_vendoring_package_owns_its_workspace(self):
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = []\n')
        run_cargo = subprocess.run

        def cargo_vendor(command, **kwargs):
            manifest = Path(command[command.index("--manifest-path") + 1])
            result = run_cargo(
                ["cargo", "locate-project", "--workspace", "--message-format", "plain",
                 "--manifest-path", str(manifest)],
                check=True, capture_output=True, text=True)
            self.assertEqual(Path(result.stdout.strip()), manifest)
            return self.cargo_vendor(command, **kwargs)

        with patch.object(prepare.subprocess, "run", side_effect=cargo_vendor):
            prepare.vendor_wgpu(self.root, self.packages, "0.3.28", False)

    def test_new_release_replaces_previous_generated_output(self):
        with patch.object(prepare.subprocess, "run", side_effect=self.cargo_vendor):
            for version in ("0.3.26", "0.3.27"):
                with self.subTest(version=version):
                    manifests = prepare.vendor_wgpu(self.root, self.packages, version, False)
                    manifest = self.vendor / "wgpu" / "Cargo.toml"
                    self.assertEqual(manifests, {"wgpu": manifest})
                    self.assertIn(f'version = "{version}"', manifest.read_text())
                    self.assertFalse((self.vendor / "previous-release").exists())
                    self.assertFalse((manifest.parent / ".cargo-checksum.json").exists())
                    self.assertIn("WGPU upstream", (manifest.parent / "README.md").read_text())

    def test_failed_generation_preserves_previous_output(self):
        with patch.object(prepare.subprocess, "run", side_effect=self.cargo_vendor), patch.object(
                prepare, "rewrite_wgpu", side_effect=ValueError("invalid manifest")):
            with self.assertRaisesRegex(ValueError, "invalid manifest"):
                prepare.vendor_wgpu(self.root, self.packages, "0.3.26", False)
        self.assertEqual(list(self.vendor.iterdir()), [self.vendor / "previous-release"])
        self.assertEqual((self.vendor / "previous-release").read_text(), "preserve until success")
        self.assertEqual(list(self.vendor.parent.iterdir()), [self.vendor])

    def test_dry_run_preserves_previous_output_without_running_cargo(self):
        with patch.object(prepare.subprocess, "run") as run:
            prepare.vendor_wgpu(self.root, self.packages, "0.3.26", True)
        run.assert_not_called()
        self.assertEqual(list(self.vendor.iterdir()), [self.vendor / "previous-release"])
        self.assertEqual(list(self.vendor.parent.iterdir()), [self.vendor])


if __name__ == "__main__":
    unittest.main()
