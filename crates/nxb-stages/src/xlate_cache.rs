//! The in-process translator of the shader step (`nxb_xlate::msl::translate`) in front of the store (docs/early-cutoff.md).
//! A translation is kept under the translator identity, the job, the code bytes and what the stage step adds (chain,
//! source, paired shader, the inputs of the pixel partners) and the revisions of the translator features the shader's
//! code uses (`nxb_xlate::msl::revision`): a feature bump translates only the shaders that use the feature. Every stage
//! after it keys on content again (the compile by the MSL), so equal outputs stop a change there.
//!
//! A panic of the translator is caught and counted. A failure keeps the reason string of `nxb_xlate::msl::Failure`.

use nxb_model::ContentKey;
use nxb_pyjson::{Value, dumps, parse};
use nxb_shader::job::Job;
use nxb_store::Store;
use nxb_xlate::job::Job as XJob;
use nxb_xlate::msl::revision::{self, Feature};
use nxb_xlate::msl::{Context, Failure, Side, StageInputs, Translated, Varying, translate};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// One library of a translation: the MSL and the JSON of its reflection.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lib {
    pub(crate) suffix: String,
    pub(crate) msl: String,
    pub(crate) json: Option<Value>,
}

/// A translated shader as the finish step needs it. It is what the store keeps.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Translation {
    pub(crate) stage: String,
    pub(crate) libs: Vec<Lib>,
    pub(crate) inputs: StageInputs,
    pub(crate) side: Option<Side>,
}

impl Translation {
    fn of(t: &Translated) -> Translation {
        Translation {
            stage: t.stage.clone(),
            libs: t
                .libraries
                .iter()
                .map(|l| Lib {
                    suffix: l.suffix.clone(),
                    msl: l.msl.clone(),
                    json: l.reflection.as_ref().map(|r| r.to_value()),
                })
                .collect(),
            inputs: t.inputs.clone(),
            side: t.side.clone(),
        }
    }

    fn to_text(&self) -> String {
        let strs = |v: &[String]| Value::List(v.iter().map(Value::str).collect());
        let libs = self
            .libs
            .iter()
            .map(|l| {
                Value::obj([
                    ("suffix", Value::str(&l.suffix)),
                    ("msl", Value::str(&l.msl)),
                    ("json", l.json.clone().unwrap_or(Value::Null)),
                ])
            })
            .collect();
        let varyings = self
            .inputs
            .varyings
            .iter()
            .map(|v| {
                Value::obj([
                    ("location", Value::Int(i64::from(v.location))),
                    ("type", Value::str(&v.msl_type)),
                    ("interp", Value::str(&v.interpolation)),
                    ("read", Value::Int(i64::from(v.read))),
                ])
            })
            .collect();
        let side = self.side.as_ref().map_or(Value::Null, |s| {
            Value::obj([("alpha", strs(&s.alpha)), ("logic", strs(&s.logic))])
        });
        dumps(&Value::obj([
            ("stage", Value::str(&self.stage)),
            ("libs", Value::List(libs)),
            ("varyings", Value::List(varyings)),
            ("side", side),
        ]))
    }

    fn from_text(text: &str) -> Option<Translation> {
        let v = parse(text).ok()?;
        let strs = |v: Option<&Value>| -> Option<Vec<String>> {
            v?.as_list()?
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect()
        };
        let libs = v
            .get("libs")?
            .as_list()?
            .iter()
            .map(|l| {
                Some(Lib {
                    suffix: l.get("suffix")?.as_str()?.to_string(),
                    msl: l.get("msl")?.as_str()?.to_string(),
                    json: match l.get("json")? {
                        Value::Null => None,
                        j => Some(j.clone()),
                    },
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let varyings = v
            .get("varyings")?
            .as_list()?
            .iter()
            .map(|x| {
                Some(Varying {
                    location: u32::try_from(x.get("location")?.as_i64()?).ok()?,
                    msl_type: x.get("type")?.as_str()?.to_string(),
                    interpolation: x.get("interp")?.as_str()?.to_string(),
                    read: u8::try_from(x.get("read")?.as_i64()?).ok()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let side = match v.get("side")? {
            Value::Null => None,
            s => Some(Side {
                alpha: strs(s.get("alpha"))?,
                logic: strs(s.get("logic"))?,
            }),
        };
        Some(Translation {
            stage: v.get("stage")?.as_str()?.to_string(),
            libs,
            inputs: StageInputs { varyings },
            side,
        })
    }
}

/// The translator function: job, code bytes, context.
type Run = dyn Fn(&XJob, &[u8], &Context<'_>) -> Result<Translated, Failure> + Send + Sync;

/// The in-process translator in front of the store.
pub(crate) struct Xlate {
    store: Store,
    id: String,
    run: Box<Run>,
    features: &'static [Feature],
    /// Jobs answered from the store.
    pub(crate) hits: AtomicU64,
    /// Jobs translated.
    pub(crate) misses: AtomicU64,
    refused: Mutex<BTreeMap<String, u64>>,
}

/// The translator identity: the label, the chain version V (the engine files of `dmsh_chain.py` `FILES` and the options,
/// no Rust source), the version of the MSL back end (`nxb_xlate::msl::TRANSLATOR_ID`) and the `NXB_TEST_XLATE_SALT` test
/// hook. A change of `nxb-xlate` translates again only when someone increases `TRANSLATOR_ID` (every shader) or a feature
/// revision of `nxb_xlate::msl::revision` (the shaders that use the feature) by hand. A new chain
/// translates again, and the compile cache stops the change where the MSL stays equal.
pub(crate) fn translator_id(chain: &str) -> String {
    let mut h = nxb_model::Sha256::new();
    h.update(b"nxb-xlate msl\0");
    h.update(chain.as_bytes());
    h.update(&[0]);
    h.update(nxb_xlate::msl::TRANSLATOR_ID.as_bytes());
    h.update(&[0]);
    h.update(
        std::env::var("NXB_TEST_XLATE_SALT")
            .unwrap_or_default()
            .as_bytes(),
    );
    nxb_model::to_hex(&h.finalize().0)
}

impl Xlate {
    /// The translator of a build: `nxb_xlate::msl::translate` over the store at `root`.
    pub(crate) fn open(root: &Path, chain: &str) -> Result<Xlate, String> {
        let mut store = Store::open(root).map_err(|e| e.to_string())?;
        store.set_durability(nxb_store::Durability::Relaxed);
        Ok(Xlate::with(
            store,
            translator_id(chain),
            Box::new(translate),
        ))
    }

    pub(crate) fn with(store: Store, id: String, run: Box<Run>) -> Xlate {
        Xlate {
            store,
            id,
            run,
            features: revision::FEATURES,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            refused: Mutex::new(BTreeMap::new()),
        }
    }

    /// The translator with other feature revisions (a feature bump in a test).
    #[cfg(test)]
    pub(crate) fn with_features(mut self, features: &'static [Feature]) -> Xlate {
        self.features = features;
        self
    }

    /// Keys the translator refused, by reason (`error` for a job it cannot read, `panic` for a caught panic).
    pub(crate) fn refused(&self) -> Vec<(String, u64)> {
        let m = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        m.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    fn refuse(&self, why: &str) {
        let mut m = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        *m.entry(why.to_string()).or_default() += 1;
    }

    /// The store key. `j` is the job as the translator reads it (`None`: it cannot read the job, so no feature).
    fn key(&self, job: &Job, j: Option<&XJob>, ctx: &Context<'_>) -> ContentKey {
        let partners: String = ctx
            .partners
            .iter()
            .flat_map(|p| &p.varyings)
            .map(|v| {
                format!(
                    "{} {} {} {}\n",
                    v.location, v.msl_type, v.interpolation, v.read
                )
            })
            .collect();
        let k = ContentKey::builder("nxb.xlate.v2")
            .field_str("translator", &self.id)
            .field_str("job", &dumps(&job.json))
            .field("code", &job.code)
            .field_str("chain", ctx.chain)
            .field_str("source", &ctx.source)
            .field_str("pc_paired", ctx.pc_paired)
            .field_str("no_facts", if ctx.no_facts { "1" } else { "0" })
            .field_str("dual_source", if ctx.dual_source { "1" } else { "0" })
            .field_str(
                "fetch_outputs_only",
                if ctx.fetch_outputs_only { "1" } else { "0" },
            )
            .field_str("partners", &partners);
        // no field for a shader that uses no feature: its key stays the key it had before features existed
        let tags = j.map_or_else(String::new, |j| {
            revision::tags_of(self.features, j, &job.code)
        });
        if tags.is_empty() {
            k.finish()
        } else {
            k.field_str("features", &tags).finish()
        }
    }

    /// Translate one shader, from the store when its key is there.
    pub(crate) fn translate(&self, job: &Job, ctx: &Context<'_>) -> Result<Translation, Failure> {
        let parsed = XJob::from_value(&job.json);
        let key = self.key(job, parsed.as_ref().ok(), ctx);
        let hit = self
            .store
            .lookup(&key)
            .ok()
            .flatten()
            .and_then(|m| {
                let f = m.files.iter().find(|f| f.name == "t.json")?;
                String::from_utf8(self.store.read_blob(&f.digest.sha256).ok()?).ok()
            })
            .and_then(|t| Translation::from_text(&t));
        if let Some(t) = hit {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(t);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let j = parsed.map_err(|e| {
            self.refuse("error");
            Failure::new("translation failed", e)
        })?;
        let t = match catch_unwind(AssertUnwindSafe(|| (self.run)(&j, &job.code, ctx))) {
            Err(_) => {
                self.refuse("panic");
                return Err(Failure::new(
                    "translation failed",
                    "panic in the Rust translator",
                ));
            }
            Ok(Err(f)) => {
                self.refuse(&f.reason);
                return Err(f);
            }
            Ok(Ok(t)) => Translation::of(&t),
        };
        let mut w = self.store.entry(key);
        if w.add_bytes("t.json", t.to_text().as_bytes()).is_ok() {
            let _ = w.commit();
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests;
