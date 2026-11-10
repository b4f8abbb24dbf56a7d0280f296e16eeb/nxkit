#!/usr/bin/env python3
"""F4 speed variants on one input list, N workers, alternating rounds (load average printed per run).
apple: metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -include-pch PCH (stdin) | metallib
A: airc emit -> files -> air-opt -O1 -> air-opt --discard-value-names -> metallib (4 processes, temp files)
B: the same chain through pipes (no temp files)
C: airc emit | air-as | metallib (no optimiser)
D-opt / D-direct: airc build (front end in-process, one process for all inputs; back half piped)
usage: speed2.py LIST PCH [-j 6] [--rounds 2]"""
import os, statistics, subprocess, sys, tempfile, time
from concurrent.futures import ThreadPoolExecutor
x = lambda t: subprocess.run(['xcrun', '-sdk', 'macosx', '-find', t], capture_output=True, text=True).stdout.strip()
SDK = subprocess.run(['xcrun', '-sdk', 'macosx', '--show-sdk-path'], capture_output=True, text=True).stdout.strip()
METAL, OPT, LIB, AS = x('metal'), x('air-opt'), x('metallib'), x('air-as')
AIRC = (os.environ.get('NXB_AIRC', 'target/release/airc'))
ENV = {'SDKROOT': SDK, 'PATH': '/usr/bin:/bin'}
a = sys.argv[1:]
LIST, PCH = a[0], a[1]
J = int(a[a.index('-j') + 1]) if '-j' in a else 6
R = int(a[a.index('--rounds') + 1]) if '--rounds' in a else 2
srcs = [l.strip() for l in open(LIST) if l.strip()]
tmp = tempfile.mkdtemp(prefix='speed2')

def sh(cmd):
    r = subprocess.run(cmd, shell=True, env=ENV, capture_output=True)
    if r.returncode: raise RuntimeError(r.stderr[-300:])

def q(s): return "'" + s.replace("'", "'\\''") + "'"

def apple(i):
    o = os.path.join(tmp, 'a%d' % i)
    sh('%s -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -include-pch %s -x metal - -o - < %s | %s - -o %s.lib' % (METAL, PCH, q(srcs[i]), LIB, o))

def va(i):
    o = os.path.join(tmp, 'A%d' % i)
    sh('%s emit %s -o %s.ll && %s -O1 %s.ll -o %s.o1 && %s --discard-value-names %s.o1 -o %s.air && %s %s.air -o %s.lib' % (AIRC, q(srcs[i]), o, OPT, o, o, OPT, o, o, LIB, o, o))

def vb(i):
    o = os.path.join(tmp, 'B%d' % i)
    sh('%s emit %s | %s -O1 - -o - | %s --discard-value-names - -o - | %s - -o %s.lib' % (AIRC, q(srcs[i]), OPT, OPT, LIB, o))

def vc(i):
    o = os.path.join(tmp, 'C%d' % i)
    sh('%s emit %s | %s - -o - | %s - -o %s.lib' % (AIRC, q(srcs[i]), AS, LIB, o))

def pool(fn):
    t0 = time.time(); l0 = os.getloadavg()[0]
    def timed(i):
        t = time.time(); fn(i); return time.time() - t
    with ThreadPoolExecutor(J) as ex:
        per = sorted(ex.map(timed, range(len(srcs))))
    w = time.time() - t0
    return w, statistics.median(per), per[int(len(per) * .9)], l0, os.getloadavg()[0]

for r in range(R):
    for name, fn in (('apple', apple), ('A', va), ('B', vb), ('C', vc)):
        w, med, p90, l0, l1 = pool(fn)
        print('%-8s %d in, %d procs: wall %.1f s, %.1f in/s, median %.0f ms, p90 %.0f ms, load %.1f->%.1f' % (name, len(srcs), J, w, len(srcs) / w, med * 1000, p90 * 1000, l0, l1), flush=True)
    for mode in ('', '--direct'):
        l0 = os.getloadavg()[0]
        out = subprocess.run([AIRC, 'build', '-l', LIST, '-j', str(J), '--work', tmp + '/D'] + ([mode] if mode else []), capture_output=True, text=True).stdout.strip()
        print('D%-7s %s, load %.1f->%.1f' % (mode or '-opt', out, l0, os.getloadavg()[0]), flush=True)
