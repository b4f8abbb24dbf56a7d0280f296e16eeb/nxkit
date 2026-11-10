#!/usr/bin/env python3
"""End-to-end compile speed, own path vs Apple's, over a list of MSL inputs with N worker processes.
own:   airc emit -> air-opt -O1 -> air-opt --discard-value-names -> metallib
apple: metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv [-include-pch PCH] (source on stdin) -> metallib
usage: speed.py LIST [-j 6] [--pch PCH]   Prints wall time, inputs/s, per-input median and p90, load average."""
import os, statistics, subprocess, sys, tempfile, time
from concurrent.futures import ThreadPoolExecutor
x = lambda t: subprocess.run(['xcrun', '-sdk', 'macosx', '-find', t], capture_output=True, text=True).stdout.strip()
SDK = subprocess.run(['xcrun', '-sdk', 'macosx', '--show-sdk-path'], capture_output=True, text=True).stdout.strip()
METAL, OPT, LIB = x('metal'), x('air-opt'), x('metallib')
AIRC = (os.environ.get('NXB_AIRC', 'target/release/airc'))
ENV = {'SDKROOT': SDK, 'PATH': '/usr/bin:/bin'}
args = sys.argv[1:]
J = int(args[args.index('-j') + 1]) if '-j' in args else 6
PCH = args[args.index('--pch') + 1] if '--pch' in args else None
srcs = [l.strip() for l in open(args[0]) if l.strip()]
tmp = tempfile.mkdtemp(prefix='speed')

def run(cmd, stdin=None):
    r = subprocess.run(cmd, env=ENV, stdin=stdin, capture_output=True)
    if r.returncode:
        raise RuntimeError(r.stderr[-200:])

def own(i):
    p = lambda e: os.path.join(tmp, 'o%d.%s' % (i, e))
    t = time.time()
    run([AIRC, 'emit', srcs[i], '-o', p('ll')])
    run([OPT, '-O1', p('ll'), '-o', p('o1')])
    run([OPT, '--discard-value-names', p('o1'), '-o', p('air')])
    run([LIB, p('air'), '-o', p('lib')])
    return time.time() - t

def apple(i):
    p = lambda e: os.path.join(tmp, 'a%d.%s' % (i, e))
    t = time.time()
    with open(srcs[i], 'rb') as f:
        run([METAL, '-c', '-O1', '-fmetal-math-mode=relaxed', '-fpreserve-invariance', '-fwrapv', *(['-include-pch', PCH] if PCH else []), '-x', 'metal', '-', '-o', p('air')], f)
    run([LIB, p('air'), '-o', p('lib')])
    return time.time() - t

for name, fn in (('apple', apple), ('own', own), ('apple', apple), ('own', own)):
    t0 = time.time(); l0 = os.getloadavg()[0]
    with ThreadPoolExecutor(J) as ex:
        per = list(ex.map(fn, range(len(srcs))))
    wall = time.time() - t0; l1 = os.getloadavg()[0]
    per.sort()
    print('%-5s %d inputs, %d procs: wall %.1f s, %.1f inputs/s, median %.0f ms, p90 %.0f ms, load %.1f -> %.1f' % (
        name, len(srcs), J, wall, len(srcs) / wall, 1000 * statistics.median(per), 1000 * per[int(len(per) * 0.9)], l0, l1))
