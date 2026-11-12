"""Compile the shell's shared C headers without requiring Apple SDKs."""
import os
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ShellHeaderTests(unittest.TestCase):
    def test_overlay_uses_workspace_host_abi(self):
        cc = shutil.which(os.environ.get('CC', 'cc'))
        if not cc:
            self.skipTest('A C compiler is required')
        # Include both routes, as the Swift bridging header does. This checks
        # relative header resolution, the include guard, and the shared ABI.
        source = '''#include "../../../crates/nxkit-host/include/nxkit_ios.h"
#include "overlay/Nx2ipaOverlay.h"
void check_host_abi(void) {
    Nx2ipaTouch touch = { .id = 1, .x = 0.5f, .y = 0.5f };
    nx2ipa_touches(&touch, 1);
    nx2ipa_virtual_pad(0, 0, 0, 0, 0);
    (void)nx2ipa_has_controller();
    (void)nx2ipa_is_handheld();
    (void)nx2ipa_picture_aspect();
}
'''
        subprocess.run(
            [cc, '-x', 'c', '-std=c11', '-Wall', '-Wextra', '-Werror',
             '-fsyntax-only', '-I', str(ROOT / 'shell'), '-'],
            input=source, text=True, check=True, capture_output=True,
        )


if __name__ == '__main__':
    unittest.main()
