"""Exercise the actual C stderr capture used by Swift, without Apple SDKs."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class LaunchLogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        cls.directory = Path(cls.temp.name)
        source = cls.directory / 'capture.c'
        source.write_text('''#include "LaunchLog.h"
#include <stdlib.h>
int main(int argc, char **argv) {
    if (argc != 2) return 3;
    int saved = nxkit_open_launch_log(argv[1]);
    if (saved < 0) {
        fputs("capture failed\\n", stderr);
        return 2;
    }
    fputs("startup stage\\n", stderr);
    if (write(STDERR_FILENO, "fatal last line\\n", 16) != 16) return 4;
    if (getenv("FINALIZE")) {
        if (nxkit_close_launch_log(saved) != 0) return 5;
        fputs("after startup\\n", stderr);
        if (nxkit_trim_launch_log(argv[1], 16) != 0) return 6;
    }
    exit(1);
}
''')
        cls.binary = cls.directory / 'capture'
        cc = shutil.which(os.environ.get('CC', 'cc'))
        if not cc:
            raise unittest.SkipTest('A C compiler is required')
        subprocess.run([cc, '-std=c11', '-D_POSIX_C_SOURCE=200809L', '-Wall', '-Wextra', '-Werror',
                        '-I', str(ROOT / 'shell'), str(source), '-o', str(cls.binary)], check=True)

    def test_fatal_last_line_and_private_permissions(self):
        log = self.directory / 'launch.log'
        result = subprocess.run([self.binary, log], capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, b'')
        self.assertEqual(log.read_bytes(), b'startup stage\nfatal last line\n')
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)

    def test_new_launch_truncates_old_bytes(self):
        log = self.directory / 'truncate.log'
        log.write_text('old data' * 100)
        subprocess.run([self.binary, log], capture_output=True)
        self.assertEqual(log.read_bytes(), b'startup stage\nfatal last line\n')

    def test_failure_leaves_stderr_available(self):
        result = subprocess.run([self.binary, self.directory / 'missing' / 'log'], capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr, b'capture failed\n')

    def test_finalize_restores_stderr_and_retains_tail(self):
        log = self.directory / 'finalize.log'
        result = subprocess.run([self.binary, log], capture_output=True,
                                env={**os.environ, 'FINALIZE': '1'})
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, b'after startup\n')
        self.assertEqual(log.read_bytes(), b'fatal last line\n')



if __name__ == '__main__':
    unittest.main()
