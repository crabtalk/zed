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
        result = prepare.rewrite_wgpu(text, "wgpu", packages, "cddb2f5abcdef")
        self.assertIn('[lib]\nname = "wgpu"', result)
        self.assertIn('package = "bezel-wgpu-core"', result)
        self.assertIn('version = "=30.0.0-bezel.rcddb2f5abcdef"', result)
        self.assertIn('features = ["webgl"]\noptional = true', result)
        self.assertIn('[dependencies.log]\nversion = "0.4"', result)
        self.assertNotIn("naga-test", result)

    def test_dependency_outside_the_wgpu_closure_fails(self):
        with self.assertRaisesRegex(SystemExit, "escapes WGPU closure"):
            prepare.rewrite_wgpu(
                '[dependencies.other]\npath = "../other"\n', "wgpu",
                {"wgpu": {"version": "30.0.0"}}, "cddb2f5abcdef")

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


if __name__ == "__main__":
    unittest.main()
