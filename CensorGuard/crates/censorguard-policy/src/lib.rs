//! Strict YAML parsing and deterministic policy compilation.

mod compiler;
mod config;
mod dns;
mod error;
mod evaluator;
mod net;
mod overlay;

pub use compiler::{
    CompileWarning, CompiledPolicy, CompiledRules, RuleValue, compile, compile_yaml, load,
};
pub use config::{DomainRef, GroupPolicy, PolicyRoot};
pub use dns::{DnsCache, DnsResolution};
pub use error::PolicyError;
pub use evaluator::evaluate_intents;
pub use net::{DomainNetworkRule, NetworkAction};
