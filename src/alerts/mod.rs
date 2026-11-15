//! Alerts: rules ("loss above 5 % over 2 minutes on these targets") checked against the running traces, with the
//! state of each alert (when it started, its worst value) and route change detection. Pure logic: the app feeds it
//! what the tracers measured and acts on the events it returns (notifications, the journal, timeline marks).

mod engine;
mod route_watch;
mod rules;

pub use self::engine::{Engine, Event, Observe, WindowStats};
pub use self::rules::{Condition, ConditionKind, Rule, Targets, span, unit_label};
