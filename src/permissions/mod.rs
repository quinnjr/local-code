pub mod gate;
pub mod settings;
pub mod stdio;
pub mod types;

pub use gate::{CheckOutcome, PermissionGate};
pub use settings::{PermissionSettings, SettingsFile, load_settings};
pub use stdio::StdioPrompter;
pub use types::{
    PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier, ToolKind,
    classify_tool,
};
