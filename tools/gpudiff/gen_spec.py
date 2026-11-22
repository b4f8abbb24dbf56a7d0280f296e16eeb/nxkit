#!/usr/bin/env python3
"""Writes the gpudiff spec of one shader: what to run for every entry point and how to fill every input.

    python3 -I gen_spec.py <shader.metal> <reference.metallib> <out.spec.json> [--max-fc N] [--json sidecar.json]

Two sources of truth:
  * the reference metallib's reflection (gpudiff reflect): entry kinds, function constants, vertex attributes, and the
    argument buffer and buffer layouts with every member's type, from Metal's pipeline reflection
  * the MSL source (msl_info.py): stage_in / output structs (Metal does not reflect fragment stage I/O), colour inputs,
    depth output, the values function constants are compared with, and the data-flow slice that marks loop- and
    index-controlling inputs
The spec carries no data: gpudiff generates every value from (seed, vector, resource name), identically for A and B.
"""
import sys, os, re, json, random, subprocess, itertools, hashlib, collections
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import msl_info as M
import augment as AUG

HERE = os.path.dirname(os.path.abspath(__file__))
GPUDIFF = os.path.join(HERE, 'bin', 'gpudiff')
VERTS, INSTANCES = 96, 2

# Engine ABI structs (dm/gfx/tools: dmsh_alpha, dmsh_logic, dmsh_pull, dmsh_geometry, nvn_border): valid random contents
def known_struct_rules(tname, verts, instances, src=''):
    if tname == 'dm_alpha_state':
        # the reference often equals the alpha the shader writes (0, 1, 0.5, texel values): boundaries get tested
        return [{'path': 'func', 'class': 'ctl', 'lo': 0, 'hi': 9},
                {'path': 'ref', 'class': 'choice', 'repr': 'f', 'values': [0.0, 1.0, 0.5, 0.25, 0.75, 0.0, 1.0, 0.003921569, 0.996078431, 0.1, -1.0, 2.0]}]
    if tname == 'dm_logic_state':
        return [{'path': 'op', 'class': 'ctl', 'lo': 0, 'hi': 17}, {'path': 'pad0', 'class': 'zero'}, {'path': 'pad1', 'class': 'zero'}, {'path': 'pad2', 'class': 'zero'},
                {'path': 'mask[*]', 'class': 'choice', 'values': [0, 255, 1023, 65535, 1, 3, 15, 31, 63, 4294967295, 0, 255]}]
    if tname == 'dm_vlayout':
        # NVN attribute words that decode to real data: formats the shader's own dm_ftab knows (float32 most often, as
        # in real meshes, because random bytes read as half or float give NaN and hide everything), offsets aligned to 4
        fmts = vertex_formats(src)
        return [{'path': 's[*].stride', 'class': 'choice', 'values': [16, 16, 32, 32, 48, 64, 0, 12, 20, 24, 28]},
                {'path': 's[*].divisor', 'class': 'choice', 'values': [0, 0, 0, 0, 0, 0, 0, 0, 1, 2]},
                {'path': 'attr[*]', 'class': 'fields', 'fields': [{'shift': 0, 'bits': 8, 'values': fmts},
                                                                  {'shift': 8, 'bits': 19, 'values': [0, 0, 4, 8, 12, 16, 32]},
                                                                  {'shift': 27, 'bits': 5, 'lo': 0, 'hi': 4}]}]
    if tname == 'dm_geometry_capture':
        return [{'path': 'info.x', 'class': 'const', 'value': 0}, {'path': 'info.y', 'class': 'const', 'value': verts},
                {'path': 'info.z', 'class': 'const', 'value': 0}, {'path': 'info.w', 'class': 'const', 'value': instances},
                {'path': 'stride', 'class': 'const', 'value': 144}, {'path': 'slot[*]', 'class': 'iota'}]
    if tname == 'dm_border_t':
        return [{'path': 'mask', 'class': 'bits'}, {'path': 'k[*]', 'class': 'frange', 'lo': 0, 'hi': 1}]
    return []


def vertex_formats(src):
    m = re.search(r'dm_ftab\[256\]\s*=\s*\{([^}]*)\}', src)
    if not m: return list(range(256))
    tab = [int(x) for x in re.findall(r'\d+', m.group(1))]
    good, f32 = [], []
    for i, t in enumerate(tab):
        if not t: continue
        kind, sz = t >> 4, (t >> 2) & 3
        if kind == 5 and sz == 2: f32.append(i)                 # float32
        elif kind in (1, 2, 8, 9, 11): good.append(i)  # unorm, snorm, packed normalized: finite and in [-1, 1]
        # integer formats (3, 4) and integer-to-float (6, 7) turn random bits into huge values: rare in the mix
    others = [i for i, t in enumerate(tab) if t and (t >> 4) in (3, 4, 6, 7, 10)]
    if f32: return f32 * 6 + good * 2 + others
    return good or list(range(256))


def reflect(lib, partial):
    tmp = partial + '.tmp'
    r = subprocess.run([GPUDIFF, 'reflect', lib, '--spec', tmp], capture_output=True, text=True, timeout=300)
    if r.returncode != 0: raise RuntimeError('gpudiff reflect failed: ' + r.stderr[-800:])
    return json.loads(r.stdout)


def fc_sets(decl, compared, rnd, max_sets):
    """function constant combinations: every combination when few, else all-on, all-off, one per compared int value,
    and seeded random ones, deduplicated"""
    bools = [d for d in decl if d['type'] == 'bool']
    ints = [d for d in decl if d['type'] != 'bool']
    ivals = {}
    for d in ints:
        vals = sorted(set(compared.get(d['name'], [])) | {0})
        vals.append(max(vals) + 1)
        ivals[d['name']] = vals
    total = 2 ** len(bools) * (1 if not ints else eval('*'.join(str(len(v)) for v in ivals.values())))
    out = []
    if total <= max_sets:
        for bs in itertools.product([True, False], repeat=len(bools)):
            for iv in itertools.product(*[ivals[d['name']] for d in ints]) if ints else [()]:
                s = {d['name']: b for d, b in zip(bools, bs)}
                s.update({d['name']: v for d, v in zip(ints, iv)})
                out.append(s)
        return out
    def add(s):
        if s not in out: out.append(s)
    max_sets = min(12, max(max_sets, 1 + sum(len(v) for v in ivals.values())))
    first_i = {k: v[0] for k, v in ivals.items()}
    add({**{d['name']: True for d in bools}, **first_i})
    for name, vals in ivals.items():
        for v in [vals[-1]] + vals[:-1]:
            if len(out) >= max_sets: break
            s = {d['name']: rnd.random() < 0.7 for d in bools}
            s.update({k: rnd.choice(vv) for k, vv in ivals.items()}); s[name] = v
            # a bool whose name ends with the selected value (FB_<v> for FX_0 == v) is switched on
            for d in bools:
                if re.search(r'_%d$' % v, d['name']) and d['name'].startswith('FB'): s[d['name']] = True
            add(s)
    if len(out) < max_sets: add({**{d['name']: False for d in bools}, **first_i})
    tries = 0
    while len(out) < max_sets and tries < 100:
        tries += 1
        s = {d['name']: rnd.random() < 0.6 for d in bools}
        s.update({k: rnd.choice(vv) for k, vv in ivals.items()})
        add(s)
    return out[:max_sets]


def user_attr(attrs):
    for a in attrs:
        if a.startswith('user('): return a
    return None


def make_partial(info, sidecar, max_fc, seed):
    structs = info['structs']
    rnd = random.Random(seed)
    compared = {d['name']: d['compared'] for d in info['function_constants']}
    structs_entries = info['entries']
    entries = []
    for e in info['entries']:
        ent = {'name': e['name'], 'kind': e['kind']}
        params = e['params']
        used_buffers = {int(M.attr_val(p['attrs'], 'buffer')) for p in params if M.attr_val(p['attrs'], 'buffer') not in (None, '')}
        if e['kind'] == 'kernel':
            tg = (sidecar or {}).get('threadgroup_size') or [8, 1, 1]
            n = max(1, tg[0] * tg[1] * tg[2])
            ent.update(mode='compute', tg=tg, groups=[max(1, min(8, 512 // n)), 1, 1])   # up to 512 threads: still tiny
        elif e['kind'] == 'vertex':
            sin = [p for p in params if 'stage_in' in p['attrs']]
            ent['attr_buffer'] = max(i for i in range(1, 31) if i not in used_buffers)
            ent['seed_group'] = 'vertex'
            if e['ret'] == 'void':
                cap = [p for p in params if 'dm_geometry_capture' in p['type']]
                ent['mode'] = 'capture' if cap else 'noraster'
                # a few vertices and one instance more than the capture range: the engine's range guard runs both ways
                ent['draw'] = {'verts': VERTS + 8, 'instances': INSTANCES + 1}
                if cap:
                    ent['capture'] = {'region': 'buf:vertex:%s.dst' % cap[0]['name'], 'stride': 144, 'pos': 0, 'verts': VERTS, 'instances': INSTANCES}
                elif e['name'].endswith('_hcap'):
                    of = next((x for x in structs_entries if x['name'] == e['name'][:-5]), None)
                    if of:
                        words, pos = 0, None
                        for m in structs.get(of['ret'], []):
                            wt = AUG.words_of(m['type'])
                            if not wt: continue
                            if any(a.startswith('position') for a in m['attrs']): pos = words
                            words += wt[1] * (m['array'] or 1)
                        if pos is not None:
                            ent['capture'] = {'region': 'buf:vertex:dm_hcap', 'stride': words, 'pos': pos, 'verts': VERTS, 'instances': INSTANCES, 'rows': AUG.MAXV}
            else:
                st = structs.get(e['ret'], [])
                ent['mode'] = 'raster'
                if any('render_target_array_index' in a for m in st for a in m['attrs']): ent['points'] = True
                ent['vout'] = [{'name': m['name'], 'type': m['type'], 'attr': user_attr(m['attrs']) or ','.join(m['attrs'])} for m in st if not m['array']]
                ent['draw'] = {'verts': VERTS, 'instances': 1}
            ent['has_stage_in'] = bool(sin)
        else:
            ent['mode'] = 'fragment'
            sin = [p for p in params if 'stage_in' in p['attrs']]
            stage_in = []
            if sin:
                for m in structs.get(sin[0]['type'], []):
                    ua = user_attr(m['attrs'])
                    if not ua: continue
                    item = {'name': m['name'], 'type': m['type'], 'attr': ua, 'interp': [a for a in m['attrs'] if a != ua]}
                    ctl = info['stage_ctl'].get(m['name'])
                    if ctl: item.update(ctl)
                    stage_in.append(item)
            ent['stage_in'] = stage_in
            targets = {}
            depth = None
            for m in structs.get(e['ret'], []):
                c = M.attr_val(m['attrs'], 'color')
                if c is not None and c != '': targets[int(c.split(',')[0])] = m['type']
                if any(a.startswith('depth') for a in m['attrs']): depth = {'write': True}
            for p in params:
                c = M.attr_val(p['attrs'], 'color')
                if c is not None and c != '': targets.setdefault(int(c.split(',')[0]), p['type'])
            if not targets: targets[0] = 'float4'
            ent['targets'] = [{'index': k, 'type': v} for k, v in sorted(targets.items())]
            if e['early_fragment_tests']: depth = dict(depth or {}, early=True)
            if depth: ent['depth'] = depth
            if any('point_coord' in p['attrs'] for p in params): ent['points'] = True
        # function constants this entry references: all declared ones the signature or the file uses
        decl = info['function_constants']
        ent['fc_sets'] = fc_sets(decl, compared, rnd, max_fc) if decl else []
        entries.append(ent)
    return entries


def main(argv):
    src_path, lib, out = argv[1], argv[2], argv[3]
    max_fc = 6
    sidecar = None
    if '--max-fc' in argv: max_fc = int(argv[argv.index('--max-fc') + 1])
    if '--json' in argv: sidecar = json.load(open(argv[argv.index('--json') + 1]))
    elif src_path.endswith('_b.metal') and os.path.exists(src_path[:-6] + '.json'):
        sidecar = json.load(open(src_path[:-6] + '.json'))
    src = open(src_path, errors='replace').read()
    info = M.analyse(src)
    if any(e['kind'] == 'kernel' for e in info['entries']):
        for rules in info['hints'].values():
            for r in rules:
                if r.get('class') == 'ctl' and not r.get('loop') and r.get('hi') == 16: r['hi'] = 64
    name = os.path.basename(src_path)
    seed = int(hashlib.sha1(name.encode()).hexdigest()[:8], 16)
    entries = make_partial(info, sidecar, max_fc, seed)
    with open(out + '.tmp', 'w') as f: json.dump({'name': name, 'entries': entries}, f)
    refl = reflect(lib, out)
    os.unlink(out + '.tmp')
    fns = {f['name']: f for f in refl['functions']}
    hints = {k: list(v) for k, v in info['hints'].items()}
    samplers, textures, pointer_sizes, problems = {}, {}, {}, []
    depth_tex = set()
    arg_types = {}
    for f in refl['functions']:
        for b in f.get('bindings', []):
            lay = b.get('layout') or {}
            if b.get('argbuf'):
                for m in lay.get('members', []):
                    arg_types[m['name']] = m
                    if m['type'] == 'texture' and m.get('depth'): depth_tex.add(m['name'])
        if 'bindings_error' in f: problems.append('%s: %s' % (f['name'], f['bindings_error']))
    for nm, m in arg_types.items():
        if m['type'] == 'sampler':
            base = re.sub(r'Smplr(_w)?$', '', nm)
            s = {}
            if nm.endswith('_w') and nm[:-2] in arg_types: s['twin_of'] = 'arg:' + nm[:-2]
            if base in depth_tex or nm in info['compare_samplers']: s['compare'] = True
            if s: samplers['arg:' + nm] = s
    for t in info['texture_ctl']: textures['arg:' + t] = {'class': 'ctl'}
    # engine ABI structs, by the type of the argument buffer member or buffer parameter
    for a in info['argbuf']:
        t = a['type'].replace('constant', '').replace('device', '').replace('*', '').strip()
        r = known_struct_rules(t, VERTS, INSTANCES, src)
        if r: hints['arg:' + a['name']] = r + hints.get('arg:' + a['name'], [])
    for e in info['entries']:
        for p in e['params']:
            t = p['type'].replace('constant', '').replace('device', '').replace('&', '').strip()
            r = known_struct_rules(t, VERTS, INSTANCES, src)
            if r:
                hints['buf:' + p['name']] = r + [h for h in hints.get('buf:' + p['name'], []) if h['path'] not in {x['path'] for x in r}]
            if t == 'dm_geometry_capture':
                pointer_sizes['buf:vertex:%s.dst' % p['name']] = (VERTS + 8) * (INSTANCES + 1) * 144 * 4 + 4096
    buffer_sizes = {}
    if any(ent['name'].endswith('_hcap') for ent in entries): buffer_sizes['dm_hcap'] = 128 * (INSTANCES + 1) * 256 * 4
    for ent in entries:
        f = fns.get(ent['name'])
        if not f: problems.append('entry %s not in library' % ent['name']); continue
        if f['kind'] != ent['kind']: problems.append('entry %s kind %s vs %s' % (ent['name'], f['kind'], ent['kind']))
        if ent['kind'] == 'vertex' and f.get('vertex_attributes'):
            attrs = []
            for va in f['vertex_attributes']:
                a = {'index': va['index'], 'type': va['type'], 'dt': va['dt'], 'name': va['name']}
                ctl = info['stage_ctl'].get(va['name'])
                if ctl: a.update(ctl)
                attrs.append(a)
            ent['vertex_attrs'] = attrs
        ent.pop('has_stage_in', None)
        if ent.get('capture'):    # captures compared with each other only when their vertex inputs are the same
            ent['capture']['group'] = json.dumps(sorted((x['index'], x['type']) for x in ent.get('vertex_attrs', [])))
        # function constants: only those this entry's function declares (Metal reflection), combinations recomputed
        own = {c['name'] for c in f.get('function_constants', [])}
        decl = [d for d in info['function_constants'] if d['name'] in own]
        compared = {d['name']: d['compared'] for d in decl}
        r_fc = random.Random(seed ^ int(hashlib.sha1(ent['name'].encode()).hexdigest()[:6], 16))
        sets = fc_sets(decl, compared, r_fc, max_fc) if decl else []
        # outputs that share a colour index under different constants (OM_i_j: output i to target j) exclude each other
        groups = collections.defaultdict(list)
        e_src = next((e for e in info['entries'] if e['name'] == ent['name']), None)
        if e_src:
            for m in info['structs'].get(e_src['ret'], []):
                c, fcn = M.attr_val(m['attrs'], 'color'), M.attr_val(m['attrs'], 'function_constant')
                if c not in (None, '') and fcn: groups[c.split(',')[0]].append(fcn)
        out_sets = []
        for st in sets:
            st = dict(st)
            # FX_n = v reads colour input v (fetch variants): the input exists only with FB_v on, as in the engine
            for k, v in list(st.items()):
                if not isinstance(v, bool) and isinstance(v, int) and ('FB_%d' % v) in st: st['FB_%d' % v] = True
            for names in groups.values():
                on = [n for n in names if st.get(n)]
                if len(on) > 1:
                    keep = r_fc.choice(on)
                    for n in on: st[n] = (n == keep)
            if st not in out_sets: out_sets.append(st)
        ent['fc_sets'] = out_sets
    tail = sorted(k for k, rs in hints.items() if k.startswith('arg:') and any(r.get('loop') and '*' in r['path'] for r in rs))
    spec = {'name': name, 'source': os.path.abspath(src_path), 'reference': os.path.abspath(lib), 'entries': entries,
            'hints': hints, 'samplers': samplers, 'textures': textures, 'pointer_sizes': pointer_sizes, 'tail_regions': tail, 'buffer_sizes': buffer_sizes,
            'facts': {'loops': info['loops'], 'loop_slice': info['n_loop_slice'], 'index_slice': info['n_idx_slice']},
            'problems': problems}
    with open(out, 'w') as f: json.dump(spec, f, indent=1)
    return 0 if not problems else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
