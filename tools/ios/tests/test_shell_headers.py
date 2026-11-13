"""Compile the shell's shared C headers without requiring Apple SDKs."""
import os
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ShellHeaderTests(unittest.TestCase):
    def test_one_touch_owner_and_no_duplicate_shell_scaffolding(self):
        shell = ROOT / 'shell'
        view = (shell / 'MetalView.swift').read_text()
        overlay = (shell / 'overlay/Gamepad.m').read_text()
        self.assertNotIn('touchesBegan', view)
        self.assertNotIn('nxkit_touches(', view)
        self.assertIn('nxkit_overlay_active(active)', view)
        self.assertIn('- (void)touchesCancelled:', overlay)
        self.assertIn('if (!CGRectEqualToRect(_touchBounds, self.bounds))', overlay)
        self.assertNotIn('DmLayout', overlay)
        for path in ['TouchState.swift', 'README.md', 'overlay/DmLayout.h']:
            self.assertFalse((shell / path).exists(), path)

    def test_frame_assets_use_bundle_directory_not_executable(self):
        host = ROOT.parents[1] / 'crates/nxkit-host/src'
        session = (host / 'session.rs').read_text()
        frames = (host / 'frames.rs').read_text()
        self.assertIn('frame_hooks(&ctx, &memory, &paths.bundle, &settings)', session)
        self.assertNotIn('paths.bundle.join("nxkit")', session)
        self.assertIn('shader_dir: root.join(SHADERS)', frames)

    def test_overlay_uses_workspace_host_abi(self):
        cc = shutil.which(os.environ.get('CC', 'cc'))
        if not cc:
            self.skipTest('A C compiler is required')
        # Include both routes, as the Swift bridging header does. This checks
        # relative header resolution, the include guard, and the shared ABI.
        source = '''#include "../../../crates/nxkit-host/include/nxkit_ios.h"
#include "overlay/NxkitOverlay.h"
void check_host_abi(void) {
    NxkitTouch touch = { .id = 1, .x = 0.5f, .y = 0.5f };
    nxkit_touches(&touch, 1);
    nxkit_virtual_pad(0, 0, 0, 0, 0);
    (void)nxkit_has_controller();
    (void)nxkit_is_handheld();
    (void)nxkit_picture_aspect();
}
'''
        subprocess.run(
            [cc, '-x', 'c', '-std=c11', '-Wall', '-Wextra', '-Werror',
             '-fsyntax-only', '-I', str(ROOT / 'shell'), '-'],
            input=source, text=True, check=True, capture_output=True,
        )


if __name__ == '__main__':
    unittest.main()
