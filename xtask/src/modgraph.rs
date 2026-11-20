//! The graph of the top-level modules of one crate.
//!
//! An edge goes from top-level module `a` to `b` when a file of `a` names `crate::b` (also in a `crate::{..}` group),
//! names an item that the crate root re-exports from `b`, or reaches `b` through enough `super::`. The walk starts at
//! `src/lib.rs` (else `src/main.rs`) and follows the `mod` items, so `#[path]` files count and test code does not: a
//! module or item under `#[cfg(test)]` and a file with `#![cfg(test)]` are left out. Comments and strings are ignored.

use crate::lex::{Tok, tokens};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Where an edge was seen: the file and the line.
pub(crate) type Site = (PathBuf, usize);

/// The top-level modules of a crate and the edges between them.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    /// The top-level modules (test modules left out).
    pub(crate) modules: BTreeSet<String>,
    /// Each edge with every place that makes it.
    pub(crate) edges: BTreeMap<(String, String), Vec<Site>>,
}

/// A path that leaves the current module: `crate::..` (`up` is `None`) or `super::` repeated `up` times.
struct Ref {
    up: Option<usize>,
    names: Vec<String>,
    inline: Vec<String>,
    line: usize,
}

/// A `mod name;` item that loads a file.
struct ModDecl {
    name: String,
    path: Option<String>,
    inline: Vec<String>,
}

/// What one file holds.
#[derive(Default)]
struct Parsed {
    test_file: bool,
    mods: Vec<ModDecl>,
    inline_tops: Vec<String>,
    refs: Vec<Ref>,
    /// Root `use` items: (first path segment, the name the item gets).
    uses: Vec<(String, String)>,
}

/// Builds the graph of the crate whose sources are in `src`. A crate without `lib.rs` or `main.rs` has no modules.
pub(crate) fn graph(src: &Path) -> Graph {
    let root = [src.join("lib.rs"), src.join("main.rs")]
        .into_iter()
        .find(|p| p.exists());
    let Some(root) = root else {
        return Graph::default();
    };
    let Ok(text) = fs::read_to_string(&root) else {
        return Graph::default();
    };
    let top = parse(&text, true);
    let mut g = Graph::default();
    g.modules.extend(top.mods.iter().map(|m| m.name.clone()));
    g.modules.extend(top.inline_tops.iter().cloned());
    let reexports: BTreeMap<String, String> = top
        .uses
        .iter()
        .filter(|(first, _)| g.modules.contains(first))
        .map(|(first, leaf)| (leaf.clone(), first.clone()))
        .collect();
    let mut w = Walk {
        reexports,
        g: &mut g,
    };
    w.file(&root, true, &[], &top);
    g
}

struct Walk<'a> {
    reexports: BTreeMap<String, String>,
    g: &'a mut Graph,
}

impl Walk<'_> {
    fn file(&mut self, file: &Path, mod_rs: bool, modpath: &[String], p: &Parsed) {
        for r in &p.refs {
            let full: Vec<String> = modpath.iter().chain(&r.inline).cloned().collect();
            let Some(from) = full.first() else { continue };
            let at_root = match r.up {
                None => true,
                Some(k) => k == full.len(),
            };
            if !at_root {
                continue;
            }
            for n in &r.names {
                let to = if self.g.modules.contains(n) {
                    Some(n.clone())
                } else {
                    self.reexports.get(n).cloned()
                };
                if let Some(to) = to.filter(|t| t != from) {
                    let site = (file.to_path_buf(), r.line);
                    self.g
                        .edges
                        .entry((from.clone(), to))
                        .or_default()
                        .push(site);
                }
            }
        }
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        for m in &p.mods {
            let mut base = if mod_rs {
                dir.clone()
            } else {
                dir.join(file.file_stem().unwrap_or_default())
            };
            base.extend(&m.inline);
            let (child, child_mod_rs) = match &m.path {
                Some(rel) if m.inline.is_empty() => (dir.join(rel), true),
                Some(rel) => (base.join(rel), true),
                None => {
                    let flat = base.join(format!("{}.rs", m.name));
                    if flat.exists() {
                        (flat, false)
                    } else {
                        (base.join(&m.name).join("mod.rs"), true)
                    }
                }
            };
            let Ok(text) = fs::read_to_string(&child) else {
                continue;
            };
            let cp = parse(&text, false);
            if cp.test_file {
                continue;
            }
            let path: Vec<String> = modpath
                .iter()
                .chain(&m.inline)
                .chain([&m.name])
                .cloned()
                .collect();
            self.file(&child, child_mod_rs, &path, &cp);
        }
    }
}

fn ident(t: Option<&(Tok, usize)>) -> Option<&str> {
    match t {
        Some((Tok::Ident(s), _)) => Some(s),
        _ => None,
    }
}

fn is(t: Option<&(Tok, usize)>, c: char) -> bool {
    matches!(t, Some((Tok::Punct(p), _)) if *p == c)
}

/// The index after the group that opens at `i` (`{`, `(` or `[`).
fn skip_group(t: &[(Tok, usize)], i: usize) -> usize {
    let mut depth = 0i32;
    let mut k = i;
    while k < t.len() {
        match t[k].0 {
            Tok::Punct('{' | '(' | '[') => depth += 1,
            Tok::Punct('}' | ')' | ']') => {
                depth -= 1;
                if depth <= 0 {
                    return k + 1;
                }
            }
            _ => {}
        }
        k += 1;
    }
    k
}

/// The index after the item that starts at `i`: up to a `;` or past the first `{..}` at depth 0.
fn skip_item(t: &[(Tok, usize)], i: usize) -> usize {
    let mut k = i;
    while k < t.len() {
        match t[k].0 {
            Tok::Punct(';') => return k + 1,
            Tok::Punct('{') => return skip_group(t, k),
            Tok::Punct('(' | '[') => k = skip_group(t, k),
            _ => k += 1,
        }
    }
    k
}

/// The first segments of a path that starts at `i` (just after `::`): one ident, or the first ident of each element
/// of a `{..}` group.
fn path_heads(t: &[(Tok, usize)], i: usize) -> Vec<String> {
    if let Some(n) = ident(t.get(i)) {
        return vec![n.to_string()];
    }
    if !is(t.get(i), '{') {
        return Vec::new();
    }
    let end = skip_group(t, i);
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, true);
    for (tok, _) in &t[i + 1..end.saturating_sub(1)] {
        match tok {
            Tok::Punct('{' | '(' | '[') => depth += 1,
            Tok::Punct('}' | ')' | ']') => depth -= 1,
            Tok::Punct(',') if depth == 0 => {
                start = true;
                continue;
            }
            Tok::Ident(n) if start && depth == 0 && n != "self" => out.push(n.clone()),
            _ => {}
        }
        start = false;
    }
    out
}

/// The leaves of a `use` tree from `i` to the `;`: (first segment, bound name). A leaf is a name followed by `,`,
/// `}` or `;`, so an `as` alias wins over the name it renames.
fn use_leaves(t: &[(Tok, usize)], i: usize) -> Vec<(String, String)> {
    let end = skip_item(t, i);
    let body = &t[i..end];
    let first = body.iter().find_map(|(tok, _)| match tok {
        Tok::Ident(s) if s != "crate" && s != "self" => Some(s.clone()),
        _ => None,
    });
    let Some(first) = first else {
        return Vec::new();
    };
    body.windows(2)
        .filter_map(|w| match (&w[0].0, &w[1].0) {
            (Tok::Ident(n), Tok::Punct(',' | '}' | ';')) if n != "self" => Some(n.clone()),
            _ => None,
        })
        .map(|leaf| (first.clone(), leaf))
        .collect()
}

/// What an attribute at `i` (`#[..]` or `#![..]`) says: (is a test cfg, a `path` value, index after it).
fn attribute(t: &[(Tok, usize)], i: usize) -> (bool, Option<String>, usize) {
    let open = if is(t.get(i + 1), '!') { i + 2 } else { i + 1 };
    let end = skip_group(t, open);
    let body = &t[open + 1..end.saturating_sub(1).max(open + 1)];
    let has = |w: &str| {
        body.iter()
            .any(|(tok, _)| matches!(tok, Tok::Ident(s) if s == w))
    };
    let test = ident(body.first()) == Some("cfg") && has("test") && !has("not");
    let path = (ident(body.first()) == Some("path"))
        .then(|| {
            body.iter().find_map(|(tok, _)| match tok {
                Tok::Str(s) => Some(s.clone()),
                _ => None,
            })
        })
        .flatten();
    (test, path, end)
}

fn parse(text: &str, root: bool) -> Parsed {
    let t = tokens(text);
    let mut p = Parsed::default();
    let mut inline: Vec<(String, i32)> = Vec::new();
    let (mut depth, mut i) = (0i32, 0usize);
    let (mut pending_test, mut pending_path) = (false, None::<String>);
    while i < t.len() {
        if is(t.get(i), '#') && (is(t.get(i + 1), '[') || is(t.get(i + 1), '!')) {
            let inner = is(t.get(i + 1), '!');
            let (test, path, end) = attribute(&t, i);
            if inner && test && depth == 0 {
                p.test_file = true;
                return p;
            }
            if !inner {
                pending_test |= test;
                pending_path = path.or(pending_path);
            }
            i = end;
            continue;
        }
        if pending_test {
            pending_test = false;
            pending_path = None;
            i = skip_item(&t, i);
            continue;
        }
        let names: Vec<String> = inline.iter().map(|(n, _)| n.clone()).collect();
        match &t[i].0 {
            Tok::Ident(kw) if kw == "mod" => {
                if let Some(name) = ident(t.get(i + 1)) {
                    if is(t.get(i + 2), ';') {
                        let path = pending_path.take();
                        p.mods.push(ModDecl {
                            name: name.to_string(),
                            path,
                            inline: names,
                        });
                        i += 3;
                        continue;
                    }
                    if is(t.get(i + 2), '{') {
                        if root && depth == 0 {
                            p.inline_tops.push(name.to_string());
                        }
                        depth += 1;
                        inline.push((name.to_string(), depth));
                        i += 3;
                        pending_path = None;
                        continue;
                    }
                }
            }
            Tok::Ident(kw) if kw == "use" && root && depth == 0 => {
                p.uses.extend(use_leaves(&t, i + 1));
            }
            Tok::Ident(kw) if kw == "crate" && matches!(t.get(i + 1), Some((Tok::Path, _))) => {
                let line = t[i].1;
                let names_at = path_heads(&t, i + 2);
                p.refs.push(Ref {
                    up: None,
                    names: names_at,
                    inline: names,
                    line,
                });
            }
            Tok::Ident(kw)
                if kw == "super"
                    && !matches!(i.checked_sub(1).map(|k| &t[k].0), Some(Tok::Path)) =>
            {
                let line = t[i].1;
                let mut k = i;
                let mut up = 0;
                while ident(t.get(k)) == Some("super")
                    && matches!(t.get(k + 1), Some((Tok::Path, _)))
                {
                    up += 1;
                    k += 2;
                }
                if up > 0 {
                    let heads = path_heads(&t, k);
                    p.refs.push(Ref {
                        up: Some(up),
                        names: heads,
                        inline: names,
                        line,
                    });
                    i = k;
                    continue;
                }
            }
            Tok::Punct('{') => depth += 1,
            Tok::Punct('}') => {
                if inline.last().is_some_and(|(_, d)| *d == depth) {
                    inline.pop();
                }
                depth -= 1;
            }
            _ => {}
        }
        pending_path = None;
        i += 1;
    }
    p
}

#[cfg(test)]
#[path = "modgraph_tests.rs"]
mod tests;
