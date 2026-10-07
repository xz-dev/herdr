use serde::{Deserialize, Serialize};

use super::agents::AgentInfo;
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;

/// Boot-scoped terminal-bell facts. Absent means no bell has been recorded
/// in this runtime boot; old readers that do not know the field decode the
/// snapshot exactly as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalBellFacts {
    pub boot_id: String,
    #[serde(default)]
    pub latest_by_tab: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_bells: Option<TerminalBellFacts>,
}
