#!/usr/bin/env python3
"""Makes the rewrite rules of Breath of the Wild for the title module from nxnative's patches.py at pull request 76.

Usage: gen_botw_rules.py <nxnative dm/tools> <patches.py> <out.toml> <evidence text>

The recorder is the one of nxkit tools/gen_rewrite_patches.py (symbolic addresses of the patch area, encoders that
take an address replaced by recorders), with one more field in the context, `keys`, which the pull request's stubs fill
(VAR_STEP, RENDER_POOL, VIEW_W, VIEW_H, OUTPUT_W, OUTPUT_H). The writer follows crates/nxb-rewrite/src/rules/write.rs.
Every [[rule]] gets an `evidence` key that the reader ignores.
"""
import importlib.util, json, sys, types

tools, patches, out, evidence = sys.argv[1:5]
sys.path.insert(0, tools)
import nso_asm


class Sym:
    def __init__(self, kind, label, delta=0): self.kind, self.label, self.delta = kind, label, delta
    def __add__(self, n):
        if not isinstance(n, int): raise TypeError('Sym + non-int')
        return Sym(self.kind, self.label, self.delta + n)
    __radd__ = __add__
    def __eq__(self, o):
        if not isinstance(o, Sym) or (o.kind, o.label) != (self.kind, self.label): raise TypeError('Sym == other base')
        return o.delta == self.delta
    def __hash__(self): return hash((self.kind, self.label, self.delta))
    def __sub__(self, n):
        if isinstance(n, Sym):
            if (n.kind, n.label) != (self.kind, self.label): raise TypeError('Sym - Sym of another label')
            return self.delta - n.delta
        if not isinstance(n, int): raise TypeError('Sym - non-int')
        return Sym(self.kind, self.label, self.delta - n)
    def text(self): return ('S' if self.kind == 'S' else 'L%d' % self.label) + '%+d' % self.delta


def sym(x):
    if isinstance(x, Sym): return x.text()
    if isinstance(x, bool) or not isinstance(x, int): raise TypeError('not an address: %r' % (x,))
    return str(x)


class Tok:
    def __init__(self, op, *a, orr=0): self.op, self.a, self.orr = op, a, orr
    def __or__(self, m):
        if not isinstance(m, int): raise TypeError('Tok | non-int')
        return Tok(self.op, *self.a, orr=self.orr | m)
    __ror__ = __or__


_real = {k: getattr(nso_asm, k) for k in ('ADRP', 'B')}
def ADRP(dd, pc, target):
    if isinstance(pc, Sym) or isinstance(target, Sym): return Tok('adrp', dd, sym(pc), sym(target))
    return _real['ADRP'](dd, pc, target)
def B(frm, to):
    if isinstance(frm, Sym) or isinstance(to, Sym): return Tok('b', sym(frm), sym(to))
    return _real['B'](frm, to)
nso_asm.ADRP, nso_asm.B = ADRP, B

spec = importlib.util.spec_from_file_location('botw_patches', patches)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def q(s):
    # The reader's mini TOML: a basic string with the escapes of write.rs `quote`.
    return json.dumps(s, ensure_ascii=False)


def lst(v): return '[' + ', '.join(q(x) for x in v) + ']'


HEADER = "# nxb rewrite rules (generated from the engine's patch files). Private data: instruction words of game code.\n"
t = [HEADER, '\nscope = "01007EF00011E000"\n']
assert len(m.CUTS) == 1 and len(m.FRAME_STEP) == 1
bid = next(iter(m.CUTS))
assert bid in m.FRAME_STEP
t.append('\n[[build]]\nrow = ' + lst(['build', 'v1114112', '1.9.0', 'main=' + bid, 'subsdk0=f39f4c431234c72eb520cfdd1001bbcf', 'sdk=574284b40c06cf1821a44857939083be']) + '\n')
ev = 'evidence = ' + q(evidence) + '\n'
for off, ob, nb, why in m.CUTS[bid]:
    t.append('\n[[rule]]\nbinding = "build"\nkind = "cut"\nbuild = %s\noffset = %d\nold = %s\nnew = %s\nwhy = %s\n%s' % (q(bid), off, q(ob), q(nb), q(why), ev))

ops, nemit, keys = [], [0], {}
def here(): return Sym('L', 0, 4 * nemit[0])
def emit(w):
    nemit[0] += 1
    if isinstance(w, Tok):
        if w.op == 'adrp': ops.append(('adrp', w.a[0], w.a[1], w.a[2]))
        else: ops.append(('b', w.a[0], w.a[1], w.orr))
    elif isinstance(w, int) and not isinstance(w, bool): ops.append(('word', w))
    else: raise TypeError('not a word: %r' % (w,))
def site(off, orig): ops.append(('site', off, orig))
def put(off, words, why):
    items = []
    for w in words:
        if isinstance(w, Tok):
            assert w.op == 'b', w.op
            items.append('b:%s:%s:%d' % (w.a[0], w.a[1], w.orr))
        else: items.append('w:%d' % w)
    ops.append(('put', off, why, items))
ctx = types.SimpleNamespace(here=here, emit=emit, site=site, put=put, state=Sym('S', 0), render_scale=0, keys=keys)
ops.append(('label', 0))
m.FRAME_STEP[bid](ctx)
if ctx.render_scale: ops.append(('scale',))
t.append('\n[[rule]]\nbinding = "build"\nkind = "frame-step"\nbuild = %s\nwhy = "frame step stub of the build"\n%s' % (q(bid), ev))
for op in ops:
    t.append('\n[[rule.step]]\n')
    k = op[0]
    if k == 'site': t.append('op = "site"\nat = %d\noriginal = %s\n' % (op[1], q(op[2])))
    elif k == 'word': t.append('op = "word"\nword = %d\n' % op[1])
    elif k == 'adrp': t.append('op = "adrp"\nreg = %d\npc = %s\ntarget = %s\n' % (op[1], q(op[2]), q(op[3])))
    elif k == 'b': t.append('op = "b"\nfrom = %s\nto = %s\nmask = %d\n' % (q(op[1]), q(op[2]), op[3]))
    elif k == 'label': t.append('op = "label"\nn = %d\n' % op[1])
    elif k == 'scale': t.append('op = "scale"\n')
    elif k == 'put': t.append('op = "put"\nat = %d\nwhy = %s\nitems = %s\n' % (op[1], q(op[2]), lst(op[3])))
open(out, 'w').write(''.join(t))
print('steps', len(ops), 'words', nemit[0], 'keys', keys, file=sys.stderr)
