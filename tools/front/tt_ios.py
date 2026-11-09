#!/usr/bin/env python3
"""iOS GPU machine code of the own path (airc bc --ios | air-opt -O1 --discard-value-names) against Apple's iOS AIR
(metal -c with the engine's iOS flags), via A3's tt_compare (metal-tt render pipeline at -ios-version-min 26.0, the oldest a Metal 4 pipelines script takes; vertex _bp main0 + fragment
_bu main0, --platform ios). usage: tt_ios.py N ARCH [seed]   (ARCH e.g. applegpu_g16p)"""
import os, random, subprocess, sys, tempfile
sys.path.insert(0, ((os.environ.get('NXB_REF') or sys.exit('set NXB_REF')))); sys.dont_write_bytecode = True
import tt_compare as T
x = lambda t: subprocess.run(['xcrun', '-sdk', 'iphoneos', '-find', t], capture_output=True, text=True).stdout.strip()
SDK = subprocess.run(['xcrun', '-sdk', 'iphoneos', '--show-sdk-path'], capture_output=True, text=True).stdout.strip()
OPT = x('air-opt'); AIRC = (os.environ.get('NXB_AIRC', 'target/release/airc'))
ENV = {'SDKROOT': SDK, 'PATH': '/usr/bin:/bin'}
IOS = ['xcrun', '-sdk', 'iphoneos', 'metal', '-c', '-O1', '-mios-version-min=18.0', '-fmetal-math-mode=relaxed', '-fpreserve-invariance', '-fwrapv', '-x', 'metal', '-']
N, ARCH = int(sys.argv[1]), sys.argv[2]; rnd = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 1)
L = open(((os.environ.get('NXB_CORPUS') or sys.exit('set NXB_CORPUS')))).read().split()
vs = [p for p in L if p.endswith('_bp.metal')]; fs = [p for p in L if p.endswith('_bu.metal')]
rnd.shuffle(vs); rnd.shuffle(fs)
d = tempfile.mkdtemp()

def own(src, tag):
    bc, out = os.path.join(d, tag + '.bc'), os.path.join(d, tag + '.air')
    subprocess.run([AIRC, 'bc', '--ios', src, '-o', bc], check=True, capture_output=True)
    subprocess.run([OPT, '-O1', '--discard-value-names', bc, '-o', out], env=ENV, check=True, capture_output=True)
    return out

def apple(src, tag):
    out = os.path.join(d, tag + '.ref.air')
    subprocess.run(IOS + ['-o', out], stdin=open(src, 'rb'), check=True, capture_output=True)
    return out

same = diff = air_same = 0
for i in range(N * 4):
    if same + diff >= N: break
    v, f = vs[i], fs[i]
    try:
        va, fa, vr, fr = own(v, 'v%d' % i), own(f, 'f%d' % i), apple(v, 'v%d' % i), apple(f, 'f%d' % i)
        if open(va, 'rb').read() == open(vr, 'rb').read() and open(fa, 'rb').read() == open(fr, 'rb').read():
            air_same += 1; continue   # identical AIR: identical machine code, nothing to learn
        r = T.compare_pair(vr, va, fr, fa, arch=ARCH, osmin='26.0', platform='ios')
    except T.NotPossible as e:
        if i < 4: print('not possible', i, str(e)[:160])
        continue
    except Exception as e:
        print('skip', i, str(e)[:160]); continue
    same += bool(r['code_equal']); diff += not r['code_equal']
    print('pair %d: machine code identical %s' % (same + diff, r['code_equal']), flush=True)
print('iOS %s: pairs with differing AIR: machine code identical %d, differs %d (pairs with identical AIR skipped: %d)' % (ARCH, same, diff, air_same))
