//! Terminal-bell companion bookkeeping for client-owned shells.
//!
//! Authoritative time is server dispatch / committed focus. `latest_by_tab`
//! holds one boot-scoped global dispatch serial per live stable tab id, and
//! each shell connection keeps a cumulative `focused_through` frontier for
//! the tabs it has actually selected while surface-active. A background-tab
//! marker is owed exactly when `latest > focused_through` for that
//! connection. The original audible forwarding to the foreground client is
//! untouched by this module.

use super::*;
use crate::api::schema::TerminalBellFacts;

/// Boot-scoped terminal-bell facts shared across the runtime.
#[derive(Debug, Default)]
pub(super) struct TerminalBellsState {
    /// Global dispatch serial; incremented once per dispatched nonzero BEL batch.
    sequence: u64,
    /// Latest dispatched serial per live stable tab id. Absent means no bell
    /// in this boot. The whole live-tab domain is projected on removal so an
    /// omitted id can safely mean authoritative tab removal.
    latest_by_tab: HashMap<String, u64>,
}

impl TerminalBellsState {
    /// Advance shared facts for a BEL dispatched from `tab_id`. Returns the
    /// new serial for the emitting tab.
    pub(super) fn record_bell(&mut self, tab_id: &str) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        let serial = self.sequence;
        self.latest_by_tab.insert(tab_id.to_owned(), serial);
        serial
    }

    pub(super) fn latest_for(&self, tab_id: &str) -> u64 {
        self.latest_by_tab.get(tab_id).copied().unwrap_or(0)
    }

    /// Drop facts for stable tab ids that no longer exist authoritatively.
    pub(super) fn retain_live_tabs(&mut self, live: &HashSet<String>) {
        self.latest_by_tab.retain(|tab_id, _| live.contains(tab_id));
    }
}

impl HeadlessServer {
    /// Record a dispatched BEL in the shared facts and in the public snapshot
    /// state, then advance matching viewers' frontiers.
    ///
    /// `pane_id` resolves to at most one live tab; an orphan pane cannot
    /// produce guessed tab identity and is skipped. A zero count never
    /// reaches this method.
    pub(super) fn record_terminal_bell(&mut self, pane_id: crate::layout::PaneId) {
        let Some(tab_id) = self.terminal_bell_tab_id_for_pane(pane_id) else {
            return;
        };
        let serial = self.terminal_bells.record_bell(&tab_id);
        self.app.state.terminal_bells = Some(TerminalBellFacts {
            boot_id: self.client_shell_boot_id.clone(),
            latest_by_tab: self.shared_terminal_bells_latest(),
        });

        // Advance the frontier of every surface-active shell connection whose
        // committed tab is the emitting tab. The entire tab suppresses,
        // including non-focused or zoom-hidden split panes; outer OS focus is
        // irrelevant here. Every connection with acknowledgement state owes a
        // companion republication because shared latest changed.
        for client in self.clients.values_mut() {
            let viewing = client.is_active_shell_client()
                && client
                    .shell_location
                    .as_ref()
                    .and_then(crate::server::clients::ClientShellLocation::focused_tab_id)
                    .is_some_and(|focused| focused == tab_id.as_str());
            let Some(ack) = client.terminal_bells.as_mut() else {
                continue;
            };
            if viewing {
                ack.focused_through_by_tab.insert(tab_id.clone(), serial);
            }
            ack.dirty = true;
        }
    }

    /// Resolve the emitting pane to its stable public tab id.
    fn terminal_bell_tab_id_for_pane(&self, pane_id: crate::layout::PaneId) -> Option<String> {
        let (ws_idx, _) = self.app.find_pane(pane_id)?;
        let tab_idx = self.app.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
        self.app.public_tab_id(ws_idx, tab_idx)
    }

    /// The authoritative live stable tab id domain.
    fn live_tab_ids(&self) -> HashSet<String> {
        let mut live = HashSet::new();
        for ws_idx in 0..self.app.state.workspaces.len() {
            let tab_count = self.app.state.workspaces[ws_idx].tabs.len();
            for tab_idx in 0..tab_count {
                if let Some(tab_id) = self.app.public_tab_id(ws_idx, tab_idx) {
                    live.insert(tab_id);
                }
            }
        }
        live
    }

    /// Shared latest facts restricted to the authoritative live-tab domain.
    /// Tabs without a bell this boot map to zero so the outgoing domain is
    /// complete and omission can safely mean authoritative tab removal.
    fn shared_terminal_bells_latest(&self) -> std::collections::BTreeMap<String, u64> {
        self.live_tab_ids()
            .into_iter()
            .map(|tab_id| {
                let serial = self.terminal_bells.latest_for(&tab_id);
                (tab_id, serial)
            })
            .collect()
    }

    /// Advance one connection's frontier for a tab it committed focus into,
    /// up to that tab's current latest serial. Only surface-active
    /// connections acknowledge automatically.
    pub(super) fn advance_terminal_bells_frontier(&mut self, client_id: u64, tab_id: &str) {
        let serial = self.terminal_bells.latest_for(tab_id);
        if serial == 0 {
            return;
        }
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        if !client.is_active_shell_client() {
            return;
        }
        if let Some(ack) = client.terminal_bells.as_mut() {
            let frontier = ack
                .focused_through_by_tab
                .entry(tab_id.to_owned())
                .or_insert(0);
            if *frontier < serial {
                *frontier = serial;
                ack.dirty = true;
            }
        }
    }

    /// Drop facts for stable tab ids removed from the authoritative topology.
    pub(super) fn prune_terminal_bells_for_topology(&mut self) {
        let live = self.live_tab_ids();
        self.terminal_bells.retain_live_tabs(&live);
        for client in self.clients.values_mut() {
            if let Some(ack) = client.terminal_bells.as_mut() {
                let before = ack.focused_through_by_tab.len();
                ack.focused_through_by_tab
                    .retain(|tab_id, _| live.contains(tab_id));
                let domain_changed = ack.published.as_ref().is_some_and(|published| {
                    published.latest_by_tab.len() != live.len()
                        || published
                            .latest_by_tab
                            .keys()
                            .any(|tab_id| !live.contains(tab_id))
                });
                if ack.focused_through_by_tab.len() != before || domain_changed {
                    ack.dirty = true;
                }
            }
        }
        let latest_facts = self.shared_terminal_bells_latest();
        if let Some(facts) = self.app.state.terminal_bells.as_mut() {
            facts.latest_by_tab = latest_facts;
        }
    }

    /// Send the reliable FIFO terminal-bells seed for a newly connected shell
    /// and initialize its acknowledgement state. A fresh client baselines the
    /// current counts from this seed. Only a surface-active connection
    /// acknowledges its committed selected tab, not every tab; an inactive
    /// reconnect must not invent a visit. A same-boot reconnect keeps its held
    /// maps via the client endpoint while a
    /// new connection id starts from this baseline. The seed is not sent
    /// through the replaceable slot, so it is delivered before any later
    /// companion update even while the writer is paused.
    pub(super) fn seed_terminal_bells_for_client(&mut self, client_id: u64) {
        let committed_tab = self
            .clients
            .get(&client_id)
            .filter(|client| client.is_active_shell_client())
            .and_then(|_| self.shell_tab_id_for_client(client_id));
        let committed_frontier = committed_tab.as_ref().map(|tab_id| {
            let serial = self.terminal_bells.latest_for(tab_id);
            (tab_id.clone(), serial)
        });
        let seed = crate::protocol::endpoint::EndpointTerminalBells {
            boot_id: self.client_shell_boot_id.clone(),
            revision: 0,
            latest_by_tab: self.shared_terminal_bells_latest(),
            focused_through_by_tab: committed_frontier
                .filter(|(_, serial)| *serial > 0)
                .into_iter()
                .collect(),
        };
        let framed = match crate::protocol::endpoint::terminal_bells_message(&seed)
            .map_err(std::io::Error::other)
            .and_then(|message| Self::frame_server_message(&message).map_err(std::io::Error::other))
        {
            Ok(framed) => framed,
            Err(err) => {
                warn!(client_id, err = %err, "failed to frame terminal-bells seed");
                return;
            }
        };
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        if !client.is_shell_client() {
            return;
        }
        let Some(writer) = client.writer.as_ref() else {
            return;
        };
        if writer.control.send(framed).is_err() {
            debug!(client_id, "terminal-bells seed dropped for closed writer");
            return;
        }
        client.terminal_bells = Some(crate::server::clients::ClientTerminalBellsAck {
            focused_through_by_tab: seed.focused_through_by_tab.clone(),
            revision: 0,
            published: Some(seed),
            dirty: false,
        });
    }

    /// Publish the current full companion facts for one connection through
    /// the replaceable bounded slot when facts changed since the last
    /// successful publication.
    ///
    /// The dirty boundary keeps unchanged renders allocation-free for this
    /// path: nothing is rebuilt or compared when no BEL or frontier change
    /// happened. Substantive facts (latest map plus frontier map, boot id) are
    /// compared against the last published projection before the revision is
    /// bumped, so equality holds when nothing changed.
    pub(super) fn publish_terminal_bells_companion(&mut self, client_id: u64) {
        // Capture the publication inputs in one scoped mutable borrow, then
        // release the connection before reading shared state and sending.
        let publish_input = {
            let Some(client) = self.clients.get_mut(&client_id) else {
                return;
            };
            if !client.is_shell_client() {
                return;
            }
            let ack = client.terminal_bells.get_or_insert_with(Default::default);
            if !ack.dirty {
                return;
            }
            let published_facts = ack.published.as_ref().map(|published| {
                (
                    published.boot_id.clone(),
                    published.latest_by_tab.clone(),
                    published.focused_through_by_tab.clone(),
                )
            });
            (
                published_facts,
                ack.focused_through_by_tab.clone(),
                ack.revision,
            )
        };
        let (published_facts, frontier, last_revision) = publish_input;

        let latest = self.shared_terminal_bells_latest();
        let boot_id = self.client_shell_boot_id.clone();
        let substantive_match = published_facts.as_ref().is_some_and(
            |(published_boot, published_latest, published_frontier)| {
                *published_boot == boot_id
                    && *published_latest == latest
                    && *published_frontier == frontier
            },
        );
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        let Some(ack) = client.terminal_bells.as_mut() else {
            return;
        };
        if substantive_match {
            ack.dirty = false;
            return;
        }
        let revision = last_revision.saturating_add(1);
        let projection = crate::protocol::endpoint::EndpointTerminalBells {
            boot_id,
            revision,
            latest_by_tab: latest,
            focused_through_by_tab: frontier,
        };
        let Some(writer) = client.writer.as_ref() else {
            return;
        };
        match crate::protocol::endpoint::terminal_bells_message(&projection)
            .map_err(std::io::Error::other)
            .and_then(|message| Self::frame_server_message(&message).map_err(std::io::Error::other))
        {
            Ok(framed) => {
                if writer.terminal_bells.try_send(framed).is_err() {
                    return;
                }
                ack.revision = revision;
                ack.published = Some(projection);
                ack.dirty = false;
            }
            Err(err) => {
                warn!(client_id, err = %err, "failed to frame terminal-bells companion");
            }
        }
    }
}
