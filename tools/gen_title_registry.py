#!/usr/bin/env python3
"""Writes the registry of the title layer from the folders: the workspace dependencies of the family and title crates,
the features and dependencies of nxtitles and the lists of its catalog() (src/families.rs, src/titles.rs).

usage: tools/gen_title_registry.py <nxkit>

The order of titles already listed is kept; new ones follow in title id order. Families are in id order, engines first."""
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])


def crate_name(d):
    return re.search(r'^name = "([^"]+)"', (d / 'Cargo.toml').read_text(), re.M)[1]


ws = (root / 'Cargo.toml').read_text()
listed = re.findall(r'^nxgame-([a-z0-9-]+) = \{ path = "titles/games/(\w{16})" \}', ws, re.M)
games = {}
for d in sorted((root / 'titles' / 'games').iterdir()):
    if (d / 'Cargo.toml').exists():
        games[d.name] = crate_name(d).removeprefix('nxgame-')
order = [tid for _, tid in listed if tid in games] + sorted(t for t in games if t not in {x for _, x in listed})
engines = sorted(d.name for d in (root / 'titles' / 'engines').iterdir() if (d / 'Cargo.toml').exists())
graphics = sorted(d.name for d in (root / 'titles' / 'graphics').iterdir() if (d / 'Cargo.toml').exists())

fams = [(f'engine-{e}', f'nxengine-{e}', f'titles/engines/{e}') for e in engines] + \
       [(f'graphics-{g}', f'nxgraphics-{g}', f'titles/graphics/{g}') for g in graphics]
tits = [(games[t], f'nxgame-{games[t]}', f'titles/games/{t}') for t in order]
feats = [f for f, _, _ in fams + tits]
assert len(set(feats)) == len(feats), 'two modules share a feature name'

# the workspace: one block from the first family or title crate to the last
block = '\n'.join(f'{c} = {{ path = "{p}" }}' for _, c, p in fams + tits)
lines = ws.split('\n')
mine = [i for i, l in enumerate(lines) if re.match(r'nx(?:family|engine|graphics|game)-', l)]
assert mine == list(range(mine[0], mine[-1] + 1)), 'the family and title crates are not one block'
ws = '\n'.join(lines[:mine[0]] + block.split('\n') + lines[mine[-1] + 1:])
ws = ws.replace('"titles/families/*"', '"titles/engines/*", "titles/graphics/*"')
(root / 'Cargo.toml').write_text(ws)

# nxtitles/Cargo.toml
p = root / 'titles' / 'nxtitles' / 'Cargo.toml'
t = p.read_text()
head = t[:t.index('[features]')]
tail = t[t.index('[lints]'):]


def wrap_list(items, indent='    ', width=120):
    out, line = [], indent
    for i in items:
        w = f'"{i}",'
        if len(line) + len(w) + 1 > width and line.strip():
            out.append(line.rstrip())
            line = indent
        line += w + ' '
    out.append(line.rstrip())
    return '\n'.join(out)


t = head + '[features]\ndefault = [\n' + wrap_list(feats) + '\n]\n'
t += ''.join(f'{f} = ["dep:{c}"]\n' for f, c, _ in fams + tits)
t += '\n[dependencies]\nnxtitle.workspace = true\n'
t += ''.join(f'{c} = {{ workspace = true, optional = true }}\n' for _, c, _ in fams + tits)
t += '\n' + tail
p.write_text(t)

# nxtitles/src/families.rs and titles.rs: the two lists of catalog()
def entries(rows, static):
    return ''.join(f'        #[cfg(feature = "{f}")]\n        &{c.replace("-", "_")}::{static(f)},\n' for f, c, _ in rows)


def listfile(name, doc, ty, items):
    (root / 'titles' / 'nxtitles' / 'src' / name).write_text(f'''//! {doc}
//!
//! Written by `tools/gen_title_registry.py` from the folders; do not edit.

use nxtitle::{ty};

/// {doc}
pub(crate) fn all() -> Vec<&'static dyn {ty}> {{
    vec![
{items}    ]
}}
''')


listfile('families.rs', 'The engine families, then the graphics families, each behind its feature.', 'Family',
         entries([r for r in fams if r[0].startswith('engine-')], lambda f: 'ENGINE') +
         entries([r for r in fams if r[0].startswith('graphics-')], lambda f: 'GRAPHICS'))
listfile('titles.rs', 'The title modules, each behind its feature.', 'Title',
         entries(tits, lambda f: f.upper().replace('-', '_')))
print(len(engines), 'engines,', len(graphics), 'graphics,', len(tits), 'titles')
