//! The catalog: families and titles, and how they are composed for one game.

use crate::{
    BuildHooks, ConfigKey, ConsoleSettings, ConsoleVar, Fact, Family, GraphicsApi, Layer,
    LimitRule, Match, Need, PaceProfile, Pacing, ResourcePatch, RomfsRead, Runtime, RuntimeHooks,
    Service, ShaderRole, ShaderView, Standalone, Title, TitleFacts, Upscaler,
};
use std::collections::BTreeSet;

/// All families and titles that are built in.
pub struct Catalog {
    families: Vec<&'static dyn Family>,
    titles: Vec<&'static dyn Title>,
}

/// How the engine family of a game was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineFrom {
    /// The title module names it.
    Declared,
    /// A family's detection said yes (a game without a title module).
    Detected,
    /// No engine family.
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

    /// The families of both layers.
    pub fn families(&self) -> &[&'static dyn Family] {
        &self.families
    }

    /// The titles.
    pub fn titles(&self) -> &[&'static dyn Title] {
        &self.titles
    }

    fn family(&self, layer: Layer, id: &str) -> Option<&'static dyn Family> {
        self.families
            .iter()
            .copied()
            .find(|f| f.layer() == layer && f.id() == id)
    }

    /// Titles that name a family this catalog does not hold, as `(title, layer, family id)`: the engine family the
    /// title names and the graphics family of its API. A build without that family's feature has such titles. They
    /// still work, with the next layer in place of the family.
    pub fn missing_families(&self) -> Vec<(&'static dyn Title, Layer, &'static str)> {
        let mut out = Vec::new();
        for t in &self.titles {
            if self.family(Layer::Engine, t.engine()).is_none() {
                out.push((*t, Layer::Engine, t.engine()));
            }
            let api = t.graphics().value;
            if self.family(Layer::Graphics, api.id()).is_none() {
                out.push((*t, Layer::Graphics, api.id()));
            }
        }
        out
    }

    /// The title module and families for one game. The engine family the title names wins. Without a title module,
    /// the first engine family whose detection says yes is taken. The graphics family is the one of the game's
    /// graphics API ([`Resolved::graphics_api`]).
    pub fn resolve(&self, facts: &TitleFacts) -> Resolved {
        let title = self
            .titles
            .iter()
            .copied()
            .find(|t| t.id() == facts.title_id);
        self.resolve_title(title, facts)
    }

    fn resolve_title(&self, title: Option<&'static dyn Title>, facts: &TitleFacts) -> Resolved {
        let (engine, from) = match title {
            Some(t) => (self.family(Layer::Engine, t.engine()), EngineFrom::Declared),
            None => (
                self.families
                    .iter()
                    .copied()
                    .filter(|f| f.layer() == Layer::Engine)
                    .find(|f| f.detect(facts) == Match::Yes),
                EngineFrom::Detected,
            ),
        };
        let engine_from = if engine.is_some() {
            from
        } else {
            EngineFrom::None
        };
        let mut r = Resolved {
            title,
            engine,
            engine_from,
            graphics: None,
        };
        r.graphics = self.family(Layer::Graphics, r.graphics_api().id());
        r
    }
}

/// The composed answers for one game: the title first, then the engine family, then the graphics family, then `None`
/// for the core default. A list adds up the other way round: the graphics family's entries, the engine family's, the
/// title's.
#[derive(Clone, Copy)]
pub struct Resolved {
    /// The title module, if one is built in.
    pub title: Option<&'static dyn Title>,
    /// The engine family, if one was found.
    pub engine: Option<&'static dyn Family>,
    /// How the engine family was found.
    pub engine_from: EngineFrom,
    /// The graphics family of the game's API, if it is built in.
    pub graphics: Option<&'static dyn Family>,
}

impl Resolved {
    fn layers(&self) -> impl Iterator<Item = &'static dyn Family> {
        self.engine.into_iter().chain(self.graphics)
    }

    fn build_hooks(&self) -> impl Iterator<Item = &'static dyn BuildHooks> {
        let t = self.title.map(|t| t as &'static dyn BuildHooks);
        t.into_iter()
            .chain(self.layers().map(|f| f as &'static dyn BuildHooks))
    }

    fn runtime_hooks(&self) -> impl Iterator<Item = &'static dyn RuntimeHooks> {
        let t = self.title.map(|t| t as &'static dyn RuntimeHooks);
        t.into_iter()
            .chain(self.layers().map(|f| f as &'static dyn RuntimeHooks))
    }

    /// The engine that runs a service: the title's choice, then the families', then [`Runtime::Engine`].
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

    /// The pipeline seed texts of the families, then the title's.
    pub fn pipeline_seeds(&self) -> Vec<&'static str> {
        let hooks: Vec<_> = self.build_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .filter_map(|h| h.pipeline_seeds())
            .collect()
    }

    /// The data of a build without the engine: the title's, then the families'.
    pub fn standalone(&self) -> Option<Standalone> {
        self.build_hooks().find_map(|h| h.standalone())
    }

    /// The graphics API: the title's answer ([`Title::graphics`]), [`GraphicsApi::Nvn`] for a game without a title
    /// module.
    pub fn graphics_api(&self) -> GraphicsApi {
        self.title.map(|t| t.graphics().value).unwrap_or_default()
    }

    /// The title's own rewriter rules (the families' are not in it), as TOML text.
    pub fn title_rules(&self) -> Option<&'static str> {
        self.title
            .map(|t| t as &'static dyn BuildHooks)
            .and_then(|h| h.rewrite_rules())
    }

    /// The rewriter rules of the families as `(family id, TOML text)`: the engine family's, then the graphics
    /// family's. The scope of each file is the family's id, so a build searches those scopes after the title's.
    pub fn family_rules(&self) -> Vec<(&'static str, &'static str)> {
        self.layers()
            .filter_map(|f| f.rewrite_rules().map(|r| (f.id(), r)))
            .collect()
    }

    /// The rewriter rules of the families, then the title's.
    pub fn rewrite_rules(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .build_hooks()
            .filter_map(|h| h.rewrite_rules())
            .collect();
        v.reverse();
        v
    }

    /// The resource patches of the families, then the title's.
    pub fn resource_patches(&self) -> Vec<Fact<ResourcePatch>> {
        let hooks: Vec<_> = self.build_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.resource_patches().iter().copied())
            .collect()
    }

    /// The console variables of the families, then the title's, as they apply: a later value of a name wins.
    pub fn console_vars(&self) -> Vec<Fact<ConsoleVar>> {
        let hooks: Vec<_> = self.build_hooks().collect();
        let all: Vec<Fact<ConsoleVar>> = hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.console_vars().iter().copied())
            .collect();
        let merged = crate::merge_console_vars(all.iter().map(|f| f.value));
        merged
            .into_iter()
            .filter_map(|v| all.iter().rev().find(|f| f.value == v).copied())
            .collect()
    }

    /// The configuration keys of the families, then the title's, as they apply: a later value of a section and key wins.
    pub fn config_keys(&self) -> Vec<Fact<ConfigKey>> {
        let hooks: Vec<_> = self.build_hooks().collect();
        let all: Vec<Fact<ConfigKey>> = hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.config_keys().iter().copied())
            .collect();
        let merged = crate::merge_config_keys(all.iter().map(|f| f.value));
        merged
            .into_iter()
            .filter_map(|k| all.iter().rev().find(|f| f.value == k).copied())
            .collect()
    }

    /// The RomFS files that set `set`: the answer of the first layer that has one (the title, then the engine family, then the graphics family).
    /// `None` when no layer knows how.
    pub fn console_var_files(
        &self,
        set: ConsoleSettings<'_>,
        romfs: &dyn RomfsRead,
    ) -> Option<Result<crate::ConsoleFiles, String>> {
        self.build_hooks()
            .find_map(|h| h.console_var_files(set, romfs))
    }

    /// The needs of the families, then the title's.
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

    /// The frame limit rules of the families, then the title's.
    pub fn limits(&self) -> Vec<Fact<LimitRule>> {
        let hooks: Vec<_> = self.runtime_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.limits().iter().copied())
            .collect()
    }

    /// Whether the game decodes the shared font with `ScalableFontEngineHelper::Decode`. The title's answer, then the
    /// families'.
    pub fn shared_font_decode(&self) -> Option<Fact<bool>> {
        self.runtime_hooks().find_map(|h| h.shared_font_decode())
    }

    /// The program arguments that set the render size. The title's answer, then the families'.
    pub fn render_args(&self) -> Option<Fact<crate::RenderArgs>> {
        self.runtime_hooks().find_map(|h| h.render_args())
    }

    /// The layout of the view uniform buffer and the velocity. The title's answer, then the families'.
    pub fn temporal_view(&self) -> Option<Fact<crate::TemporalView>> {
        self.runtime_hooks().find_map(|h| h.temporal_view())
    }

    /// The draws the temporal upscaler replaces. The title's answer, then the families'.
    pub fn temporal_draws(&self) -> Option<Fact<crate::TemporalDraws>> {
        self.runtime_hooks().find_map(|h| h.temporal_draws())
    }

    /// The services the game opens through `nn::sf::hipc`, of the families and then the title.
    pub fn sf_services(&self) -> Vec<Fact<&'static str>> {
        let hooks: Vec<_> = self.runtime_hooks().collect();
        hooks
            .into_iter()
            .rev()
            .flat_map(|h| h.sf_services().iter().copied())
            .collect()
    }
}
