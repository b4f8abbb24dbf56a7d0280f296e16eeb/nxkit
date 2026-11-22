#!/usr/bin/env python3
"""Shared glue: corpus scan, preparing a shader (augment, compile, spec), and a batch runner that survives GPU hangs
(a process that caused a GPU hang gets its later submissions ignored by the driver, so the runner restarts gpudiff
without the culprit, and gives up after a few hangs)."""
import os, re, sys, json, glob, subprocess, hashlib, random, time, collections
sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import augment as AUG

GPUDIFF = os.path.join(HERE, 'bin', 'gpudiff')
CAS = ((os.environ.get('NXB_CAS') or sys.exit('set NXB_CAS')))
PROTO = ((os.environ.get('NXB_PROTO') or sys.exit('set NXB_PROTO')))
ENTRY_RE = re.compile(r'^(?:\[\[[^\n]*?\]\]\s*)?(vertex|fragment|kernel)\s+[\w:<>, ]+?\s+(\w+)\(', re.M)


def entries_of(src):
    return sorted({'%s %s' % (k, n) for k, n in ENTRY_RE.findall(src)})


def scan_corpus(out_path):
    recs = []
    def add(set_, src, ref=None, side=None):
        try: s = open(src, errors='replace').read()
        except OSError: return
        recs.append({'id': '%s:%s' % (set_, hashlib.sha1(src.encode()).hexdigest()[:12]), 'set': set_, 'src': src, 'ref': ref, 'json': side,
                     'entries': entries_of(s), 'bytes': len(s)})
    for d in glob.glob(CAS + '/shader/*/mac'):
        for m in glob.glob(d + '/*_b.metal'):
            lib = m[:-6] + '.metallib'
            add('b', m, lib if os.path.exists(lib) else None, m[:-6] + '.json')
    for v in glob.glob(CAS + '/variant/*'):
        if os.path.exists(v + '/metal'): add('bf', v + '/metal', v + '/mac.metallib' if os.path.exists(v + '/mac.metallib') else None)
    for u in glob.glob(PROTO + '/units/*_bu.metal'): add('bu', u)
    for u in glob.glob(PROTO + '/vsvar/*.metal'): add('bp' if u.endswith('_bp.metal') else 'bv', u)
    with open(out_path, 'w') as f:
        for r in recs: f.write(json.dumps(r) + '\n')
    return recs


def load_corpus(path):
    return [json.loads(l) for l in open(path)]


def compile_msl(src, out):
    r = subprocess.run([os.path.join(HERE, 'mtlc.sh'), src, out], capture_output=True, text=True, timeout=600)
    if r.returncode != 0: raise RuntimeError('compile failed: ' + r.stderr[-600:])


def prepare(rec, wdir, force_compile=False):
    """reference metallib + spec for one corpus shader. Vertex shaders get capture twins (augment.py) and are
    compiled here; other shaders use the engine's own metallib when there is one. Returns (source used, A, spec)."""
    os.makedirs(wdir, exist_ok=True)
    key = rec['id'].replace(':', '_')
    cached = os.path.join(wdir, key + '.spec.json')
    if not force_compile and os.path.exists(cached):          # prepared before: reuse
        sp = json.load(open(cached))
        if os.path.exists(sp.get('reference', '')): return sp['source'], sp['reference'], cached
    src = rec['src']
    text = open(src, errors='replace').read()
    raster_vs = any(e.startswith('vertex ') for e in rec['entries'])
    augmented = False
    if raster_vs:
        aug, meta = AUG.augment(text)
        if meta:
            src = os.path.join(wdir, key + '.aug.metal'); augmented = True
            open(src, 'w').write(aug)
    if not augmented and not src.endswith('.metal'):        # cas/variant/<hash>/metal: a unique name for the spec seed
        dst = os.path.join(wdir, key + '.metal')
        open(dst, 'w').write(text); src = dst
    lib = rec.get('ref') if not augmented and not force_compile else None
    if not lib or not os.path.exists(lib):
        lib = os.path.join(wdir, key + '.metallib')
        compile_msl(src, lib)
    spec = os.path.join(wdir, key + '.spec.json')
    cmd = [sys.executable, '-I', os.path.join(HERE, 'gen_spec.py'), src, lib, spec]
    if rec.get('json') and os.path.exists(rec['json']): cmd += ['--json', rec['json']]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    if not os.path.exists(spec): raise RuntimeError('gen_spec failed: ' + (r.stderr or r.stdout)[-800:])
    return src, lib, spec


def run_batch(jobs, out_dir, workers=6, extra=(), max_hangs=4, log=print):
    """runs gpudiff batch over jobs ({'id','a','b','spec',...}); restarts after a GPU hang without the culprit"""
    os.makedirs(out_dir, exist_ok=True)
    results, hangs = {}, []
    pending = list(jobs)
    chunk = 0
    retried_victims = False
    t0 = time.time()
    while pending:
        chunk += 1
        jf = os.path.join(out_dir, 'jobs.%d.jsonl' % chunk)
        rf = os.path.join(out_dir, 'results.%d.jsonl' % chunk)
        with open(jf, 'w') as f:
            for j in pending: f.write(json.dumps(j) + '\n')
        # one process per worker: out-of-bounds reads of one test must never see another test's memory, and the GPU
        # address space is per process (threads of one process would read each other's arenas)
        n = max(1, min(workers, len(pending)))
        shards = [pending[i::n] for i in range(n)]
        procs = []
        for i, sh in enumerate(shards):
            sjf = jf + '.%d' % i; srf = rf + '.%d' % i
            with open(sjf, 'w') as f:
                for j in sh: f.write(json.dumps(j) + '\n')
            procs.append((subprocess.Popen([GPUDIFF, 'batch', sjf, srf, '--workers', '1', '--timeout', '2'] + list(extra),
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True), srf))
        codes, errs = [], ''
        for pr, srf in procs:
            out, err = pr.communicate()
            codes.append(pr.returncode); errs += err[-300:]
        with open(rf, 'w') as f:
            for _, srf in procs:
                if os.path.exists(srf): f.write(open(srf).read())
        class P: pass
        p = P(); p.returncode = 5 if 5 in codes else max(codes); p.stderr = errs
        got = {}
        if os.path.exists(rf):
            for l in open(rf):
                try: r = json.loads(l)
                except ValueError: continue
                got[r.get('id')] = r
        stuck = p.returncode == 5
        if not stuck:
            results.update(got)
            for j in pending:
                if j['id'] not in got: results[j['id']] = {'id': j['id'], 'status': 'error', 'error': 'no result (exit %d) %s' % (p.returncode, p.stderr[-300:])}
            # command buffers discarded because of a GPU recovery somewhere else (another process on the machine):
            # not this job's fault, run them once more
            victims = [j for j in pending if any('InnocentVictim' in (c.get('error') or '') for c in results[j['id']].get('cases', []))]
            if victims and not retried_victims:
                retried_victims = True
                log('%d jobs were victims of a GPU recovery elsewhere: running them again' % len(victims))
                pending = victims
                continue
            break
        # a hang: the culprit is the job whose own case reports "Caused GPU Hang" or a timeout. Jobs that were
        # victims (discarded command buffers, ignored submissions) or never ran go to the next chunk
        def culprit(r):
            return any(('Hang Error' in (c.get('error') or '') or 'timeout' in (c.get('error') or '')) for c in r.get('cases', []))
        suspects = [j for j in pending if j['id'] in got and culprit(got[j['id']])]
        rerun = []
        for j in pending:
            r = got.get(j['id'])
            if r is not None and r.get('status') not in ('hang', 'error') and not culprit(r): results[j['id']] = r
            elif j not in suspects: rerun.append(j)
        log('gpu hang in chunk %d: %d suspects, %d to rerun' % (chunk, len(suspects), len(rerun)))
        for j in suspects:
            if len(hangs) >= max_hangs:
                results[j['id']] = {'id': j['id'], 'status': 'skipped', 'error': 'run stopped after %d GPU hangs' % len(hangs)}
                continue
            sf = os.path.join(out_dir, 'single.jsonl'); sr = os.path.join(out_dir, 'single.result.jsonl')
            with open(sf, 'w') as f: f.write(json.dumps(j) + '\n')
            q = subprocess.run([GPUDIFF, 'batch', sf, sr, '--workers', '1', '--timeout', '2'] + list(extra), capture_output=True, text=True)
            r = json.loads(open(sr).readline() or '{}') if os.path.exists(sr) else {}
            if q.returncode == 5:
                hangs.append(j['id']); r['status'] = 'hang'; log('  hang: %s' % j['id'])
                time.sleep(2)
            r['id'] = j['id']; results[j['id']] = r
        if len(hangs) >= max_hangs:
            for j in rerun: results[j['id']] = {'id': j['id'], 'status': 'skipped', 'error': 'run stopped after %d GPU hangs' % len(hangs)}
            rerun = []
        pending = rerun
    return results, hangs, time.time() - t0
