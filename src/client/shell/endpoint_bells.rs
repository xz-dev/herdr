use super::*;

/// Per-endpoint terminal-bell presentation facts for one connection
/// generation. `seen_by_tab` is this client's acknowledgement watermark:
/// the maximum frontier it has applied per stable tab id. A background tab
/// owes a marker exactly when its shared latest serial exceeds that
/// watermark. Facts live beside `agent_presentation` because they are
/// client-local presentation state, not session truth.
#[derive(Clone, Debug, Default)]
pub(super) struct EndpointTerminalBellsPresentation {
    boot_id: Option<String>,
    generation: Option<u64>,
    revision: u64,
    latest_by_tab: std::collections::HashMap<String, u64>,
    seen_by_tab: std::collections::HashMap<String, u64>,
}

impl EndpointTerminalBellsPresentation {
    pub(super) fn receive(
        &mut self,
        generation: Option<u64>,
        bells: crate::protocol::endpoint::EndpointTerminalBells,
    ) -> bool {
        if self.generation == generation
            && self.boot_id.as_deref() == Some(bells.boot_id.as_str())
            && self.revision >= bells.revision
        {
            return false;
        }
        self.generation = generation;
        // A fresh client/new boot must not replay bells from before its seed.
        // Same-boot reconnects retain this client's existing watermarks.
        if self.boot_id.as_deref() != Some(bells.boot_id.as_str()) {
            self.seen_by_tab = bells
                .latest_by_tab
                .iter()
                .map(|(tab_id, serial)| (tab_id.clone(), *serial))
                .collect();
        }
        self.boot_id = Some(bells.boot_id);
        self.revision = bells.revision;
        self.latest_by_tab = bells.latest_by_tab.into_iter().collect();
        for (tab_id, frontier) in bells.focused_through_by_tab {
            let seen = self.seen_by_tab.entry(tab_id).or_insert(0);
            *seen = (*seen).max(frontier);
        }
        // The companion carries the complete live-tab domain, so an omitted
        // id means authoritative removal, not an unseen bell.
        self.seen_by_tab
            .retain(|tab_id, _| self.latest_by_tab.contains_key(tab_id));
        true
    }

    /// Whether `tab_id` owes a marker: a bell serial beyond this client's
    /// acknowledged frontier. Missing entries read as zero on both sides.
    pub(super) fn owes_marker(&self, tab_id: &str) -> bool {
        self.latest_by_tab.get(tab_id).copied().unwrap_or(0)
            > self.seen_by_tab.get(tab_id).copied().unwrap_or(0)
    }
}

impl ClientShellState {
    /// Install a terminal-bells companion for one endpoint and transport
    /// generation. Always stores inactive endpoint facts; returns whether
    /// the active endpoint's chrome needs repainting.
    pub(crate) fn receive_endpoint_terminal_bells(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: Option<u64>,
        bells: crate::protocol::endpoint::EndpointTerminalBells,
    ) -> bool {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return false;
        };
        let changed = endpoint.terminal_bells.receive(generation, bells);
        changed && self.endpoint_is_active(endpoint_id)
    }

    #[cfg(test)]
    pub(crate) fn test_tab_owes_terminal_bell(
        &self,
        endpoint_id: &ClientEndpointId,
        tab_id: &str,
    ) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .is_some_and(|endpoint| endpoint.terminal_bells.owes_marker(tab_id))
    }
}
