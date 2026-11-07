//! The catalog: families and titles, and how they are composed for one game.

use crate::{
    BuildHooks, Fact, Family, LimitRule, Match, Need, PaceProfile, Pacing, Runtime, RuntimeHooks,
    Seed, Service, ShaderRole, ShaderView, Standalone, Title, TitleFacts, Upscaler,
};
use std::collections::BTreeSet;

/// All families and titles that are built in.
pub struct Catalog {
    families: Vec<&'static dyn Family>,
    titles: Vec<&'static dyn Title>,
}

/// How the family of a game was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FamilyFrom {
    /// The title module names it.
    Declared,
    /// A family's detection said yes.
    Detected,
    /// No family.
    None,
}

impl Catalog {
    /// A catalog. Fails on a duplicate family id or title id.
    pub fn new(
        families: Vec<&'static dyn Family>,
        titles: Vec<&'static dyn Title>,
    ) -> Result<Catalog, String> {
        let mut ids = BTreeSet::new();
        for f in &families {
            if !ids.insert(f.id().to_string()) {
                return Err(format!("family {} is listed twice", f.id()));
            }
        }
        let mut tids = BTreeSet::new();
        for t in &titles {
            if !tids.insert(t.id()) {
                return Err(format!("title {} is listed twice", t.id()));
            }
        }
        Ok(Catalog { families, titles })
    }

    /// The families.
    pub fn families(&self) -> &[&'static dyn Family] {
        &self.families
    }

    /// The titles.
    pub fn titles(&self) -> &[&'static dyn Title] {
        &self.titles
    }

    /// Titles that name a family this catalog does not hold, as `(title, family)`. A build without that family's
    /// feature has such titles. They still work, with the core defaults in place of the family.
    pub fn missing_families(&self) -> Vec<(&'static dyn Title, &'static str)> {
        self.titles
            .iter()
            .filter_map(|t| t.family().map(|f| (*t, f)))
            .filter(|(_, f)| !self.families.iter().any(|x| x.id() == *f))
            .collect()
    }

    /// The title module and family for one game. A family the title names wins. Without one, the first family
    /// whose detection says yes is taken.
    pub fn resolve(&self, facts: &TitleFacts) -> Resolved {
        let title = self
            .titles
            .iter()
            .copied()
            .find(|t| t.id() == facts.title_id);
        let declared = title.and_then(|t| t.family());
        let (family, from) = match declared {
            Some(id) => (
                self.families.iter().copied().find(|f| f.id() == id),
                FamilyFrom::Declared,
            ),
            None => (
                self.families
                    .iter()
                    .copied()
                    .find(|f| f.detect(facts) == Match::Yes),
                FamilyFrom::Detected,
            ),
        };
        let from = if family.is_some() {
            from
        } else {
            FamilyFrom::None
        };
        Resolved {
            title,
            family,
            family_from: from,
        }
    }
}

/// The composed answers for one game: the title first, then the family, then `None` for the core default.
#[derive(Clone, Copy)]
pub struct Resolved {
    /// The title module, if one is built in.
    pub title: Option<&'static dyn Title>,
    /// The family, if one was found.
    pub family: Option<&'static dyn Family>,
    /// How the family was found.
    pub family_from: FamilyFrom,
}

impl Resolved {
    fn build_hooks(&self) -> impl Iterator<Item = &'static dyn BuildHooks> {
        let t = self.title.map(|t| t as &'static dyn BuildHooks);
        let f = self.family.map(|f| f as &'static dyn BuildHooks);
        t.into_iter().chain(f)
    }

    fn runtime_hooks(&self) -> impl Iterator<Item = &'static dyn RuntimeHooks> {
        let t = self.title.map(|t| t as &'static dyn RuntimeHooks);
        let f = self.family.map(|f| f as &'static dyn RuntimeHooks);
        t.into_iter().chain(f)
    }

    /// The engine that runs a service: the title's choice, then the family's, then [`Runtime::Nxnative`].
    pub fn runtime(&self, service: Service) -> Runtime {
        self.runtime_hooks()
            .find_map(|h| h.runtime(service))
            .map(|f| f.value)
            .unwrap_or_default()
    }

    /// The services nxkit runs for this game, in [`Service::ALL`] order.
    pub fn nxkit_services(&self) -> Vec<Service> {
        Service::ALL
            .into_iter()
            .filter(|s| self.runtime(*s) == Runtime::Nxkit)
            .collect()
    }

    /// The role of a shader.
    pub fn shader_role(&self, shader: &ShaderView<'_>) -> Option<Fact<ShaderRole>> {
        self.build_hooks().find_map(|h| h.shader_role(shader))
    }

    /// The seeds of the family, then the title's.
    pub fn seeds(&self) -> Vec<Seed> {
        let hooks: Vec<_> = self.build_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.seeds().iter().copied())
            .collect()
    }

    /// The data of a build without nxnative: the title's, then the family's.
    pub fn standalone(&self) -> Option<Standalone> {
        self.build_hooks().find_map(|h| h.standalone())
    }

    /// The title's own rewriter rules (the family's are not in it), as TOML text.
    pub fn title_rules(&self) -> Option<&'static str> {
        self.title
            .map(|t| t as &'static dyn BuildHooks)
            .and_then(|h| h.rewrite_rules())
    }

    /// The rewriter rules of the family, then the title's.
    pub fn rewrite_rules(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .build_hooks()
            .filter_map(|h| h.rewrite_rules())
            .collect();
        v.reverse();
        v
    }

    /// The needs of the family, then the title's.
    pub fn needs(&self) -> Vec<Fact<Need>> {
        let hooks: Vec<_> = self.runtime_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.needs().iter().copied())
            .collect()
    }

    /// How to handle the game's upscaler.
    pub fn upscaler(&self) -> Option<Fact<Upscaler>> {
        self.runtime_hooks().find_map(|h| h.upscaler())
    }

    /// Who paces the frames.
    pub fn pacing(&self) -> Option<Fact<Pacing>> {
        self.runtime_hooks().find_map(|h| h.pacing())
    }

    /// The keys that steer the frame step and the presents.
    pub fn pace_profile(&self) -> Option<Fact<PaceProfile>> {
        self.runtime_hooks().find_map(|h| h.pace_profile())
    }

    /// The frame limit rules of the family, then the title's.
    pub fn limits(&self) -> Vec<Fact<LimitRule>> {
        let hooks: Vec<_> = self.runtime_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.limits().iter().copied())
            .collect()
    }
}
