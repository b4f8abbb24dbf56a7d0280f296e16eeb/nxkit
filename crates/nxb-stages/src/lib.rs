//! Stage wiring: one module per stage, each building its part of the task graph from the domain crates and the runtime.
//!
//! A stage module stays under 400 lines.
//!
//! The shader step (`build`) is the one stage here. The other stages are their own L3 crates (W19).

pub mod build;
mod build_finish;
mod build_hooks;
mod build_partial;
mod build_shared;
mod build_xlate;
pub mod guard_tables;
pub mod inputs;
pub mod own_front;
pub mod own_raw;
mod xlate_cache;
