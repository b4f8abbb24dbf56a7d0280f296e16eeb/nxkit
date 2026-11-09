#!/usr/bin/env python3
"""Synthetic CRA archives and NROs for the nxb-split tests (development tool, not part of a build).

Usage: gen_cra_fixture.py <reference tools folder (the engine/dm/gfx/tools)> <crates/nxb-split/tests/data>
Needs `sumo_nvnsh.bin` in the data folder. Writes three cases, each a folder `jb_<case>/` with the files of a tree
and `jb_<case>.exp` with what the reference gives (`cra_jobs` and `cra_shaders` of shader_split.py, in the format of
tests/jobs.rs):
  * jb_cra: the archive `42`, an NRO without a file table (`a_stub.nro`) and an NRO with one (`b_game.nro`).
  * jb_cra_badsum: the same with a flipped byte in one file, so its job fails with the checksum error.
  * jb_cra_nonro: the archive alone, so the plan fails.
The NRO holds only the instruction patterns `index_of` looks for, the file table, the checksums and the suffixes.
The archive's files are scrambled by solving `unscramble` for its input (the CRC step is linear in the high word),
and the result is checked with the reference's own `read`.
"""
import hashlib, os, struct, sys, types, zlib

ref, out = sys.argv[1], sys.argv[2]
lz4 = types.ModuleType('lz4'); lz4.block = types.ModuleType('lz4.block')   # the reference imports it, no member here uses it
sys.modules['lz4'], sys.modules['lz4.block'] = lz4, lz4.block
os.environ.setdefault('DM_GAME', '0400D4201C6EA000')
sys.path.insert(0, ref)
import cra, shader_split as ss

M64 = (1 << 64) - 1
NVN = open(os.path.join(out, 'sumo_nvnsh.bin'), 'rb').read()
NVN2 = bytearray(NVN); NVN2[len(NVN2) // 2] ^= 0x5a; NVN2 = bytes(NVN2)
SUFFIXES = ['.bin', '.narc']
STEMS = {0: 'ad', 1: 'ae', 2: 'ag', 3: 'af', 4: 'ab', 5: 'ac', 6: 'aa', 7: 'al'}   # one name for each of the 8 variants
TEXT_END, TABLE = 0x1000, 0x1000


def narc(members):
    fat = b'BTAF' + struct.pack('<II', 12 + 8 * len(members), len(members))
    data, at = b'', 0
    for m in members:
        fat += struct.pack('<II', at, at + len(m)); data += m; at += len(m)
    gmif = b'GMIF' + struct.pack('<I', 8 + len(data)) + data
    return b'NARC\xfe\xff\x00\x01' + struct.pack('<IHH', 16 + len(fat) + len(gmif), 16, 2) + fat + gmif


def files():
    """(stored path with '$' for the folder name, plain bytes), in the table's order after the empty record 0"""
    fs = [
        ('shader/font/$.bin', NVN),
        ('shader/font/$_sub.narc', narc([b'\x70' + bytes(15) + NVN2, b'\x10' + bytes(15) + zlib.compress(NVN)])),
        ('sound/bgm/$.bin', b'FSTM' + bytes(12) + NVN2),   # a sound: not read
        ('movie/$.bin', b'\x1aE\xdf\xa3' + bytes(12) + NVN2),   # a movie: not read
        ('model/chr/$/$.bin', bytes(range(250)) * 3 + b'tail'),   # not a multiple of 16, no shader
    ]
    fs += [(f'data/{s}.bin', bytes((i * 7 + j) & 0xff for j in range(40 + 9 * i))) for i, s in STEMS.items()]
    fs += [('data/_x.bin', b'\x00' * 20), ('data/ab.cd_ef', b'x' * 17)]   # the stem ends at '_' (0 characters) and at '.' (2)
    return fs


def expand(stored):
    parts = []
    for c in stored.split('/'): parts.append(c.replace('$', parts[-1]) if parts else c)
    return '/'.join(parts)


def gf2_solve(cols, target):
    """The x whose bits pick the columns that xor to target"""
    basis = {}
    for j, c in enumerate(cols):
        v, mask = c, 1 << j
        while v:
            h = v.bit_length() - 1
            if h in basis: v ^= basis[h][0]; mask ^= basis[h][1]
            else: basis[h] = (v, mask); break
    v, mask = target, 0
    while v:
        h = v.bit_length() - 1
        v ^= basis[h][0]; mask ^= basis[h][1]
    return mask


def scramble(path, plain):
    """The bytes the archive holds for a file: blocks q with unscramble(q) = plain"""
    plain = plain + bytes(-len(plain) % 16)
    w = cra.key(path); v = w & 7
    tab = cra.CRC32C if v & 4 else cra.CRC32
    mul = (w & 0xfffffffe ^ cra.VARIANT_MASK[v]) << 32 | 1
    src, oth, a, b = (1, 0, 0, 1) if v & 2 else (0, 1, 0, 1)
    if v & 1: a, b = 1, 0
    cols = [cra.crc64(tab, 0, 1 << (32 + j)) for j in range(32)]
    res = bytearray()
    for i in range(0, len(plain), 16):
        o = struct.unpack_from('<2Q', plain, i)
        c = ~o[a] & 0xffffffff   # the CRC of the block's source word
        m_hi = c * (mul >> 32) & 0xffffffff
        s_lo = ((~o[a] & M64) >> 32 ^ m_hi) & 0xffffffff
        s_hi = gf2_solve(cols, c ^ cra.crc64(tab, 0, s_lo ^ w))
        m = c | m_hi << 32
        q = [0, 0]
        q[src] = s_hi << 32 | s_lo
        q[oth] = o[b] ^ (m >> 3)
        res += struct.pack('<2Q', *q)
    assert cra.unscramble(path, bytes(res))[0] == plain, path
    return bytes(res)


def checksum(path, plain):
    plain = plain + bytes(-len(plain) % 16)
    b = 0 if cra.key(path) & 1 else 1
    tot = sum(struct.unpack_from('<2Q', plain, i)[b] for i in range(0, len(plain), 16)) & M64
    return ((tot >> 32) ^ tot) & 0xffffffff


def records(stored):
    """Per record (back, prefix length, suffix index, prefix string, rest): record 0 empty"""
    recs, pres = [(0, 0, 0xff, '', '')], ['']
    for i, p in enumerate(stored, 1):
        sfx = next((j for j, s in enumerate(SUFFIXES) if p.endswith(s)), None)
        pre = p[:len(p) - len(SUFFIXES[sfx])] if sfx is not None else p
        best, bk = 0, 0
        for k in range(i):   # the record with the longest shared prefix, the latest on a tie
            n = 0
            while n < min(len(pre), len(pres[k])) and pre[n] == pres[k][n]: n += 1
            if n and n >= best: best, bk = n, k
        recs.append((i - bk if best else 0, best, 0xff if sfx is None else sfx, pre, pre[best:]))
        pres.append(pre)
    return recs


def adrp_add(at, target, reg):
    hi = (target >> 12) - (at * 4 >> 12)
    return [0x90000000 | (hi & 3) << 29 | (hi >> 2 & 0x7ffff) << 5 | reg, 0x91000000 | (target & 0xfff) << 10 | reg << 5 | reg]


def put(img, word_at, words):
    for j, w in enumerate(words): struct.pack_into('<I', img, (word_at + j) * 4, w)


def build(fs):
    """The NRO bytes and the archive bytes for a list of (stored path, plain)"""
    recs = records([s for s, _ in fs])
    count = len(recs)
    rec_at, pos = [], 0
    for _, _, _, _, rest in recs:
        rec_at.append(pos); pos += max(18, 0xd + len(rest) + 1)
    names_at = (TABLE + 2 * count + pos + 15) & ~7
    str_at = names_at + 8 * len(SUFFIXES)
    sums_at = (str_at + sum(len(s) + 1 for s in SUFFIXES) + 15) & ~15
    img = bytearray(sums_at + 4 * count + 16)
    arc = bytearray(0x2a0)
    arc[:16] = b'CRA.' + struct.pack('<HHII', 0x200, 0x100, 0x2a0, 0x2a0)   # the archive's own chunk table, which nothing reads
    info = {}
    for i, (s, plain) in enumerate(fs, 1):
        path = expand(s)
        info[i] = (len(arc), len(plain))
        arc += scramble(path, plain)
        struct.pack_into('<I', img, sums_at + 4 * i, checksum(path, plain))
    for i, (back, plen, sfx, _, rest) in enumerate(recs):
        shift = rec_at[i] - i * 18
        struct.pack_into('<h', img, TABLE + 2 * i, shift)
        r = TABLE + shift + i * 18 + 2 * count
        off, size = info.get(i, (0, 0))
        struct.pack_into('<HBB', img, r, back, plen, sfx)
        struct.pack_into('<I', img, r + 4, size ^ 0xff00ff00)
        struct.pack_into('<Q', img, r + 8, (off ^ 0xff00ff00ff) | 0xab << 40)   # the high bytes are not part of the offset
        for j, ch in enumerate(rest): img[r + 0xd + j] = ((i - ord(ch)) & 0x7f) | 0x80
    sp = str_at
    for j, s in enumerate(SUFFIXES):
        struct.pack_into('<Q', img, names_at + 8 * j, sp)
        for ch in s: img[sp] = ((j - ord(ch)) & 0x7f) | 0x80; sp += 1
        sp += 1
    mov12, ldrsh, madd = 0x52800240 | 3, 0x78e0d800 | 2 << 16 | 4 << 5, 0x1b007c00
    movn = 0x52800000 | (2 * count) << 5 | 5
    put(img, 0x440 // 4, [0, 0, mov12, ldrsh, madd, movn])   # a table pattern with no address: the next one is taken
    put(img, 0x480 // 4, adrp_add(0x480 // 4, TABLE, 4) + [mov12, ldrsh, madd, movn])
    put(img, 0x500 // 4, adrp_add(0x500 // 4, sums_at, 8) + [0, 0xd28e6aa0 | 9, 0xf2aaaca0 | 9])
    for at, tgt in ((0x580 // 4, names_at + 0x40), (0x600 // 4, names_at)):   # the last suffix pattern wins
        put(img, at, [0x39c00c00, 0x37000000, 0x92401c00] + adrp_add(at + 3, tgt, 10) + [0xf8607800 | 1 << 16])
    img[0x10:0x14] = b'NRO0'
    struct.pack_into('<I', img, 0x24, TEXT_END)
    return bytes(img), bytes(arc)


def run_reference(case_dir):
    """The expectation in the format of tests/jobs.rs, by the reference's own jobs"""
    os.environ['DM_NRO_DIR'] = case_dir
    fl = [(os.path.join(case_dir, '42'), '42')]
    jobs, arcs = ss.cra_jobs(fl)
    lines = []
    for p, r, es in jobs:
        lines.append(f'J\t{r}')
        found = []
        try:
            ss.cra_shaders(p, r, es, found)
        except Exception:
            lines.append('E'); continue
        for where, st, ctl, code in ss.uniq_keys(found).values():
            lines.append(f'K\t{where}\t{st}\t{hashlib.sha256(ctl).hexdigest()[:16]}\t{hashlib.sha256(code).hexdigest()[:16]}')
    lines.append('T\t' + ','.join(sorted(r for p, r in fl if p in arcs)))
    return '\n'.join(lines) + '\n'


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f: f.write(data)


def main():
    fs = files()
    nro, arc = build(fs)
    stub = bytearray(0x300); stub[0x10:0x14] = b'NRO0'; struct.pack_into('<I', stub, 0x24, 0x200)
    bad = bytearray(arc); bad[0x2a0 + 3] ^= 1
    for case, data in (('cra', arc), ('cra_badsum', bytes(bad)), ('cra_nonro', arc)):
        d = os.path.join(out, 'jb_' + case)
        write(os.path.join(d, '42'), data)
        if case != 'cra_nonro':
            write(os.path.join(d, 'a_stub.nro'), bytes(stub))
            write(os.path.join(d, 'b_game.nro'), nro)
    idx = cra.Index(nro)
    want = [(expand(s), len(x)) for s, x in fs]
    assert [(e[1], e[2]) for e in idx.files()] == want, [(e[1], e[2]) for e in idx.files()]
    plain = {expand(s): x for s, x in fs}
    with open(os.path.join(out, 'jb_cra', '42'), 'rb') as f:
        for e in idx.files(): assert cra.read(f, e) == plain[e[1]], e[1]
    for case in ('cra', 'cra_badsum'):
        write(os.path.join(out, f'jb_{case}.exp'), run_reference(os.path.join(out, 'jb_' + case)).encode())
    write(os.path.join(out, 'jb_cra_nonro.exp'), b'PLAN\n')
    print('variants', sorted({cra.key(p) & 7 for p, _ in want}), 'paths', [p for p, _ in want])


main()
