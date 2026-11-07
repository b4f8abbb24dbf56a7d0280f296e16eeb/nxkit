#!/usr/bin/env python3
"""Snapshot of the engine's registered rewriter patches for nxb-rewrite (development tool, not part of a build).

Usage: gen_rewrite_patches.py <engine folder holding dm/> <out.tsv>
Loads sdk_patches.py and every dm/games/*/patches.py the way dm/tools/patch_nso.py load_patches() does and writes their
CUTS, OBS, CAPABILITIES, FRAME_STEP build IDs and MATCH_CUTS rules, the build rows of every profile's game.tsv, and the
SHA-256 of each source file (path relative to dm/tools).
Games that have a title module (TITLE_MODULES) are left out: their rules are the title module's own data (R2).
The snapshot is data/rewrite-rules/snapshot.tsv. Make the rule files from it with `nxb rewrite-rules data/rewrite-rules/snapshot.tsv data/rewrite-rules`.
The patch data comes from the nxnative engine (MIT, see THIRD_PARTY.md and data/rewrite-rules/NOTICE)."""
import glob, hashlib, importlib.util, os, sys, types

engine, out = sys.argv[1], sys.argv[2]
tools = os.path.join(engine, 'dm', 'tools')
sys.path.insert(0, tools)
# FRAME_STEP stubs are code: they are run once against a recording context, with the encoders that take an address
# replaced by recorders, and stored as a trace (lines `fs`: site, word, adrp, b, label, put, scale). Addresses of the
# patch area are symbolic (Sym: a label of the emission point or the state page, plus a constant), so a stub may add
# constants to them and relocate code to them. Any other operation on a Sym (mask, shift, compare) raises, and the
# stub is stored as `fsno`: a stub the trace cannot express stays unsupported, never silently wrong.
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
TITLE_MODULES = {'01007EF00011E000'}
def has_module(path): return os.path.basename(os.path.dirname(path)) in TITLE_MODULES
files = [os.path.join(tools, 'sdk_patches.py')] + sorted(p for p in glob.glob(os.path.join(tools, '../games/*/patches.py')) if not has_module(p))
def esc(s): return str(s).replace('\\', '\\\\').replace('\t', '\\t').replace('\n', '\\n')
def row(*c): return '\t'.join(esc(x) for x in c) + '\n'
lines = []
seen = {}
for n, path in enumerate(files):
    rel = os.path.relpath(os.path.abspath(path), tools)
    lines.append(row('file', rel, hashlib.sha256(open(path, 'rb').read()).hexdigest()))
for n, path in enumerate(files):
    spec = importlib.util.spec_from_file_location(f'patches_{n}', path); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
    is_sdk = os.path.basename(path) == 'sdk_patches.py'
    for bid in sorted({b for t in ('CUTS', 'OBS', 'CAPABILITIES', 'FRAME_STEP') for b in getattr(m, t, {})}):
        lines.append(row('bscope', bid, 'sdk' if is_sdk else os.path.basename(os.path.dirname(path))))   # which patch file registered the build
    for bid, cuts in getattr(m, 'CUTS', {}).items():
        assert ('CUTS', bid) not in seen, bid; seen['CUTS', bid] = path
        for off, ob, nb, why in cuts: lines.append(row('cuts', bid, off, ob, nb, why))
        if not cuts: lines.append(row('cutsempty', bid))
    for bid, obs in getattr(m, 'OBS', {}).items():
        assert ('OBS', bid) not in seen, bid; seen['OBS', bid] = path
        for off, oid, ow, why in obs: lines.append(row('obs', bid, off, oid, ow, why))
        if not obs: lines.append(row('obsempty', bid))
    for bid, caps in getattr(m, 'CAPABILITIES', {}).items():
        for c in caps: lines.append(row('cap', bid, c))
    for bid, fn in getattr(m, 'FRAME_STEP', {}).items():
        lines.append(row('frame', bid))
        mark = len(lines)
        lines.append(row('fs', bid, 'label', 0))   # label 0: where the stub starts emitting; positions are label 0 plus 4 per word
        nemit = [0]
        def here(): return Sym('L', 0, 4 * nemit[0])
        def emit(w):
            nemit[0] += 1
            if isinstance(w, Tok):
                if w.op == 'adrp': lines.append(row('fs', bid, 'adrp', w.a[0], w.a[1], w.a[2]))
                else: lines.append(row('fs', bid, 'b', w.a[0], w.a[1], w.orr))
            elif isinstance(w, int) and not isinstance(w, bool): lines.append(row('fs', bid, 'word', w))
            else: raise TypeError('not a word: %r' % (w,))
        def site(off, orig): lines.append(row('fs', bid, 'site', off, orig))
        def put(off, words, why):
            items = []
            for w in words:
                if isinstance(w, Tok):
                    assert w.op == 'b', w.op
                    items.append('b:%s:%s:%d' % (w.a[0], w.a[1], w.orr))
                else: items.append('w:%d' % w)
            lines.append(row('fs', bid, 'put', off, why, ';'.join(items)))
        ctx = types.SimpleNamespace(here=here, emit=emit, site=site, put=put, state=Sym('S', 0), render_scale=0)
        try:
            fn(ctx)
        except Exception as e:   # arithmetic on emission points: not expressible as a trace, the rewriter reports it unsupported
            del lines[mark:]; lines.append(row('fsno', bid, repr(e)))
            continue
        if ctx.render_scale: lines.append(row('fs', bid, 'scale'))
    mc = getattr(m, 'MATCH_CUTS', {})
    if mc:
        scope = 'sdk' if is_sdk else os.path.basename(os.path.dirname(path))
        for module, rules in mc.items():
            for ri, rule in enumerate(rules):
                if 'functions' in rule:
                    assert set(rule) == {'name', 'functions'}
                    for fi, fn in enumerate(rule['functions'], 1):
                        lines.append(row('rule', scope, module, rule['name'], ri, fi, fn['size'], ','.join(fn['fingerprints'])))
                        for d, ob, nb, why in fn['cuts']: lines.append(row('rcut', scope, module, rule['name'], ri, fi, d, ob, nb, why))
                else:
                    assert set(rule) == {'name', 'size', 'fingerprints', 'cuts'}
                    lines.append(row('rule', scope, module, rule['name'], ri, 0, rule['size'], ','.join(rule['fingerprints'])))
                    for d, ob, nb, why in rule['cuts']: lines.append(row('rcut', scope, module, rule['name'], ri, 0, d, ob, nb, why))
for tsv in sorted(glob.glob(os.path.join(engine, 'dm', 'games', '*', 'game.tsv'))):
    if has_module(tsv): continue
    prof = os.path.basename(os.path.dirname(tsv))
    lines.append(row('file', os.path.relpath(tsv, tools), hashlib.sha256(open(tsv, 'rb').read()).hexdigest()))
    for r in open(tsv, encoding='utf-8').read().splitlines():
        c = [x.strip() for x in r.split('\t')]
        if c[0] == 'build' and len(c) > 3: lines.append(row('gbuild', prof, *r.split('\t')))
open(out, 'w').write(''.join(lines))
