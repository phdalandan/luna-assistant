//! Turns structured requests from the model into validated, verified Home Assistant calls.
mod execute;
mod resolve;
mod validate;

use serde::Deserialize;

pub use execute::{ExecutionReport, capitalize, execute, list, verb};
pub use resolve::{ResolveError, Selection, select};
pub use validate::{Plan, ValidationError, plan};

/// Which entities a request refers to. Every ID must come from Home Assistant metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Target {
    pub everywhere: bool,
    pub floors: Vec<String>,
    pub areas: Vec<String>,
    pub entities: Vec<String>,
    pub domains: Vec<String>,
    pub device_classes: Vec<String>,
    pub exclude_areas: Vec<String>,
    pub exclude_entities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    TurnOn,
    TurnOff,
    SetBrightness,
    SetTemperature,
    Open,
    Close,
    Lock,
    Unlock,
    Activate,
}

impl Action {
    pub const ALL: [Self; 9] = [
        Self::TurnOn,
        Self::TurnOff,
        Self::SetBrightness,
        Self::SetTemperature,
        Self::Open,
        Self::Close,
        Self::Lock,
        Self::Unlock,
        Self::Activate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::TurnOn => "turn_on",
            Self::TurnOff => "turn_off",
            Self::SetBrightness => "set_brightness",
            Self::SetTemperature => "set_temperature",
            Self::Open => "open",
            Self::Close => "close",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::Activate => "activate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub action: Action,
    pub target: Target,
    /// Brightness percentage or target temperature, depending on the action.
    #[serde(default)]
    pub value: Option<f64>,
}
