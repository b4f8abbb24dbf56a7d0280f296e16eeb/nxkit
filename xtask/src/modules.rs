//! `modules`: the ratchet on module cycles inside a crate.
//!
//! The strongly connected components (Tarjan) of each crate's module graph (refer to `modgraph`) with two or more
//! modules are the cycle groups. `xtask/module-cycles.txt` lists the groups that are accepted today. A group that is
//! not inside one listed group (a new cycle, or a group that took in a module or merged with another) fails. A listed
//! group that shrank or broke up also fails, with the lines to write instead, so the baseline only goes down.

use crate::modgraph::{Graph, graph};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The baseline file, relative to the workspace root.
pub(crate) const BASELINE: &str = "xtask/module-cycles.txt";

/// The cycle groups of one crate, each sorted, the list sorted.
pub(crate) type Groups = Vec<BTreeSet<String>>;

/// The strongly connected components with two or more modules.
pub(crate) fn cycle_groups(g: &Graph) -> Groups {
    let nodes: Vec<&String> = g.modules.iter().collect();
    let index: BTreeMap<&String, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let mut adj = vec![Vec::new(); nodes.len()];
    for (a, b) in g.edges.keys() {
        if let (Some(&x), Some(&y)) = (index.get(a), index.get(b)) {
            adj[x].push(y);
        }
    }
    let mut t = Tarjan {
        adj: &adj,
        index: vec![None; nodes.len()],
        low: vec![0; nodes.len()],
        on: vec![false; nodes.len()],
        stack: Vec::new(),
        next: 0,
        out: Vec::new(),
    };
    for v in 0..nodes.len() {
        if t.index[v].is_none() {
            t.visit(v);
        }
    }
    let mut groups: Groups = t
        .out
        .into_iter()
        .filter(|c| c.len() > 1)
        .map(|c| c.into_iter().map(|i| nodes[i].clone()).collect())
        .collect();
    groups.sort();
    groups
}

struct Tarjan<'a> {
    adj: &'a [Vec<usize>],
    index: Vec<Option<usize>>,
    low: Vec<usize>,
    on: Vec<bool>,
    stack: Vec<usize>,
    next: usize,
    out: Vec<Vec<usize>>,
}

impl Tarjan<'_> {
    fn visit(&mut self, v: usize) {
        self.index[v] = Some(self.next);
        self.low[v] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on[v] = true;
        for &w in &self.adj[v] {
            match self.index[w] {
                None => {
                    self.visit(w);
                    self.low[v] = self.low[v].min(self.low[w]);
                }
                Some(iw) if self.on[w] => self.low[v] = self.low[v].min(iw),
                Some(_) => {}
            }
        }
        if Some(self.low[v]) == self.index[v] {
            let mut comp = Vec::new();
            while let Some(w) = self.stack.pop() {
                self.on[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            self.out.push(comp);
        }
    }
}

/// Reads the baseline: `crate: a b c` per group, `#` starts a comment.
pub(crate) fn parse_baseline(text: &str) -> BTreeMap<String, Groups> {
    let mut out: BTreeMap<String, Groups> = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((krate, mods)) = line.split_once(':') else {
            continue;
        };
        let group: BTreeSet<String> = mods.split_whitespace().map(str::to_string).collect();
        out.entry(krate.trim().to_string()).or_default().push(group);
    }
    for g in out.values_mut() {
        g.sort();
    }
    out
}

/// One baseline line.
pub(crate) fn line(krate: &str, group: &BTreeSet<String>) -> String {
    let mods: Vec<&str> = group.iter().map(String::as_str).collect();
    format!("{krate}: {}", mods.join(" "))
}

/// Compares the groups of today with the baseline. Returns the errors.
pub(crate) fn compare(
    now: &BTreeMap<String, Groups>,
    base: &BTreeMap<String, Groups>,
) -> Vec<String> {
    let mut errs = Vec::new();
    let empty = Groups::new();
    let crates: BTreeSet<&String> = now.keys().chain(base.keys()).collect();
    for k in crates {
        let (n, b) = (now.get(k).unwrap_or(&empty), base.get(k).unwrap_or(&empty));
        for g in n {
            if !b.iter().any(|bg| g.is_subset(bg)) {
                errs.push(format!(
                    "new or larger module cycle: {} (break it, see `xtask graph {k}`)",
                    line(k, g)
                ));
            }
        }
        for bg in b {
            if !n.contains(bg) {
                let inside: Vec<String> = n
                    .iter()
                    .filter(|g| g.is_subset(bg))
                    .map(|g| line(k, g))
                    .collect();
                let now_text = if inside.is_empty() {
                    "no cycle left, delete the line".to_string()
                } else {
                    format!("write {}", inside.join(" and "))
                };
                errs.push(format!(
                    "a cycle got smaller, shrink {BASELINE}: {} -> {now_text}",
                    line(k, bg)
                ));
            }
        }
    }
    errs
}

/// The cycle groups of every crate in `dirs` (name, crate folder), crates without a group left out.
pub(crate) fn all_groups<'a>(
    dirs: impl Iterator<Item = (&'a str, &'a Path)>,
) -> BTreeMap<String, Groups> {
    dirs.filter_map(|(name, dir)| {
        let g = cycle_groups(&graph(&dir.join("src")));
        (!g.is_empty()).then(|| (name.to_string(), g))
    })
    .collect()
}

/// The text of `xtask graph <crate>`: the cycle groups and the edges inside them, with where each edge comes from.
pub(crate) fn explain(name: &str, dir: &Path, root: &Path) -> String {
    let g = graph(&dir.join("src"));
    let groups = cycle_groups(&g);
    let mut out = format!(
        "{name}: {} modules, {} edges, {} cycle groups\n",
        g.modules.len(),
        g.edges.len(),
        groups.len()
    );
    for grp in &groups {
        out.push_str(&format!("\ngroup of {}: {}\n", grp.len(), line(name, grp)));
        for ((a, b), sites) in &g.edges {
            if grp.contains(a) && grp.contains(b) {
                let (f, l) = &sites[0];
                let f = f.strip_prefix(root).unwrap_or(f);
                out.push_str(&format!(
                    "  {a} -> {b}: {} refs, first {}:{l}\n",
                    sites.len(),
                    f.display()
                ));
            }
        }
    }
    out
}
