// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::{EldersInfo, MemberInfo, MemberState, SectionMap, SectionMembers, SectionProofChain};
use crate::{
    consensus::AccumulatingEvent,
    id::{P2pNode, PublicId},
    location::DstLocation,
    messages::{MessageHash, SrcAuthority},
    network_params::NetworkParams,
    relocation::{self, RelocateDetails},
    xor_space::{Prefix, XorName, Xorable},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Debug,
    net::SocketAddr,
};

/// Section state that is shared among all elders of a section via Parsec consensus.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedState {
    /// Indicate whether nodes are shared state because genesis event was seen
    #[serde(skip)]
    pub handled_genesis_event: bool,
    /// Our section's key history for Secure Message Delivery
    pub our_history: SectionProofChain,
    /// Info about all members of our section.
    pub our_members: SectionMembers,
    /// Info about known sections in the network.
    pub sections: SectionMap,
    /// Backlog of completed events that need to be processed when churn completes.
    pub churn_event_backlog: VecDeque<AccumulatingEvent>,
    /// Queue of pending relocations.
    pub relocate_queue: VecDeque<RelocateDetails>,
}

impl SharedState {
    pub fn new(elders_info: EldersInfo, section_pk: bls::PublicKey) -> Self {
        Self {
            handled_genesis_event: false,
            our_history: SectionProofChain::new(section_pk),
            sections: SectionMap::new(elders_info, section_pk),
            our_members: SectionMembers::default(),
            churn_event_backlog: Default::default(),
            relocate_queue: VecDeque::new(),
        }
    }

    pub fn update(&mut self, new: Self) {
        if self.handled_genesis_event {
            log_or_panic!(
                log::Level::Error,
                "shared state update - genesis event already handled",
            );
        }

        if self.our_history.len() > 1 && *self != new {
            log_or_panic!(
                log::Level::Error,
                "shared state update - mismatch: old: {:?} --- new: {:?}",
                self,
                new
            );
        }

        *self = new;
        self.handled_genesis_event = true;
    }

    /// Returns our own current section info.
    pub fn our_info(&self) -> &EldersInfo {
        self.sections.our()
    }

    /// Returns our own current section's prefix.
    pub fn our_prefix(&self) -> &Prefix<XorName> {
        &self.our_info().prefix
    }

    /// Returns adults from our own section.
    pub fn our_adults(&self) -> impl Iterator<Item = &P2pNode> {
        self.our_members
            .mature()
            .filter(move |p2p_node| !self.is_peer_our_elder(p2p_node.name()))
    }

    /// Returns all nodes we know (our members + neighbour elders).
    pub fn known_nodes(&self) -> impl Iterator<Item = &P2pNode> {
        self.our_members
            .active()
            .map(|info| &info.p2p_node)
            .chain(self.sections.neighbour_elders())
    }

    /// Returns a section member `P2pNode`
    pub fn get_p2p_node(&self, name: &XorName) -> Option<&P2pNode> {
        self.sections
            .our()
            .elders
            .get(name)
            .or_else(|| self.our_members.get_p2p_node(name))
    }

    /// Returns whether we know the given peer.
    pub fn is_known_peer(&self, name: &XorName) -> bool {
        self.our_members.is_active(name) || self.sections.is_elder(name)
    }

    /// Checks if given name is an elder in our section or one of our neighbour sections.
    pub fn is_peer_elder(&self, name: &XorName) -> bool {
        self.sections.is_elder(name)
    }

    /// Returns whether the given peer is elder in our section.
    pub fn is_peer_our_elder(&self, name: &XorName) -> bool {
        self.our_info().elders.contains_key(name)
    }

    pub fn find_p2p_node_from_addr(&self, socket_addr: &SocketAddr) -> Option<&P2pNode> {
        self.known_nodes()
            .find(|p2p_node| p2p_node.peer_addr() == socket_addr)
    }

    /// Adds new member if its name matches our prefix and it's not already joined.
    /// Returns whether the member was actually added.
    pub fn add_member(
        &mut self,
        p2p_node: P2pNode,
        age: u8,
        recommended_section_size: usize,
    ) -> bool {
        if !self.our_prefix().matches(p2p_node.name()) {
            trace!("not adding node {} - not matching our prefix", p2p_node);
            return false;
        }

        if self.our_members.contains(p2p_node.name()) {
            trace!("not adding node {} - already a member", p2p_node);
            return false;
        }

        let name = *p2p_node.name();

        self.our_members.add(p2p_node, age);
        self.increment_age_counters(&name, recommended_section_size);

        true
    }

    /// Removes a member with the given pub_id.
    /// Returns the removed `MemberInfo` or `None` if there was no such member.
    pub fn remove_member(
        &mut self,
        pub_id: &PublicId,
        recommended_section_size: usize,
    ) -> Option<MemberInfo> {
        match self.our_members.get(pub_id.name()).map(|info| &info.state) {
            Some(MemberState::Left) | None => {
                trace!("not removing node {} - not a member", pub_id);
                return None;
            }
            Some(MemberState::Relocating { .. }) => (),
            Some(MemberState::Joined) => {
                self.increment_age_counters(pub_id.name(), recommended_section_size)
            }
        }

        self.relocate_queue
            .retain(|details| &details.pub_id != pub_id);
        self.our_members.remove(pub_id.name())
    }

    /// Find section which has member with the given id
    pub fn find_section_by_member(&self, name: &XorName) -> Option<&EldersInfo> {
        if self.our_members.contains(name) {
            Some(self.sections.our())
        } else {
            self.sections.find_neighbour_by_elder(name)
        }
    }

    /// Returns the `P2pNode` of all non-elders in the section
    pub fn adults_and_infants_p2p_nodes(&self) -> impl Iterator<Item = &P2pNode> {
        self.our_members
            .joined()
            .filter(move |info| !self.our_info().elders.contains_key(info.p2p_node.name()))
            .map(|info| &info.p2p_node)
    }

    /// Generate a new section info(s) based on the current set of members.
    /// Returns a set of EldersInfos to vote for.
    pub fn promote_and_demote_elders(
        &mut self,
        network_params: &NetworkParams,
        our_name: &XorName,
    ) -> Option<Vec<EldersInfo>> {
        if let Some((our_info, other_info)) = self.try_split(network_params, our_name) {
            return Some(vec![our_info, other_info]);
        }

        let expected_elders_map = self.elder_candidates(network_params.elder_size);
        let expected_elders: BTreeSet<_> = expected_elders_map.values().cloned().collect();
        let current_elders: BTreeSet<_> = self.our_info().elders.values().cloned().collect();

        if expected_elders == current_elders {
            None
        } else {
            let old_size = self.our_info().elders.len();

            let new_info = EldersInfo::new(
                expected_elders_map,
                self.our_info().prefix,
                self.our_info().version + 1,
            );

            if self.our_info().elders.len() < network_params.elder_size
                && old_size >= network_params.elder_size
            {
                panic!(
                    "Merging situation encountered! Not supported: {:?}",
                    self.our_info()
                );
            }

            Some(vec![new_info])
        }
    }

    pub fn update_our_section(
        &mut self,
        elders_info: EldersInfo,
        section_key: bls::PublicKey,
        signature: bls::Signature,
    ) {
        self.our_members
            .remove_not_matching_our_prefix(&elders_info.prefix);
        self.our_history.push(section_key, signature);
        self.sections.set_our(elders_info);
        self.sections
            .update_keys(self.sections.our().prefix, section_key);
    }

    pub fn poll_relocation(&mut self) -> Option<RelocateDetails> {
        // Delay relocation until all backlogged churn events have been handled. Only allow one
        // relocation at a time.
        if !self.churn_event_backlog.is_empty() {
            return None;
        }

        let details = loop {
            if let Some(details) = self.relocate_queue.pop_back() {
                if self.our_members.contains(details.pub_id.name()) {
                    break details;
                } else {
                    trace!("Not relocating {} - not a member", details.pub_id);
                }
            } else {
                return None;
            }
        };

        if self.is_peer_our_elder(details.pub_id.name()) {
            warn!(
                "Not relocating {} - The peer is still our elder.",
                details.pub_id,
            );

            // Keep the details in the queue so when the node is demoted we can relocate it.
            self.relocate_queue.push_back(details);
            return None;
        }

        trace!("relocating member {}", details.pub_id);
        Some(details)
    }

    /// Provide a SectionProofChain that proves the given signature to the given destination
    /// location.
    /// If `node_knowledge_override` is `Some`, it is used when calculating proof for
    /// `DstLocation::Node` instead of the stored knowledge. Has no effect for other location types.
    pub fn prove(
        &self,
        target: &DstLocation,
        node_knowledge_override: Option<u64>,
    ) -> SectionProofChain {
        let index = match (target, node_knowledge_override) {
            (DstLocation::Node(_), Some(knowledge)) => knowledge,
            _ => self.sections.knowledge_by_location(target),
        };

        self.our_history.slice_from(index)
    }

    /// Check if we know this node but have not yet processed it.
    pub fn is_in_online_backlog(&self, pub_id: &PublicId) -> bool {
        self.churn_event_backlog.iter().any(|evt| {
            if let AccumulatingEvent::Online(payload) = &evt {
                payload.p2p_node.public_id() == pub_id
            } else {
                false
            }
        })
    }

    /// Update our knowledge of their section and their knowledge of ours. Returns the events to
    /// vote for (if any).
    pub fn update_section_knowledge(
        &mut self,
        src: &SrcAuthority,
        dst_key: Option<&bls::PublicKey>,
        hash: &MessageHash,
    ) -> Vec<AccumulatingEvent> {
        let (&prefix, new_key) = if let Ok(pair) = src.as_section_prefix_and_key() {
            pair
        } else {
            return vec![];
        };

        let is_neighbour = self.our_prefix().is_neighbour(&prefix);

        // There will be at most two events returned because the only possible event combinations
        // are these:
        // - `[]`
        // - `[TheirKeyInfo]`
        // - `[TheirKeyInfo, TheirKnowledge]`
        // - `[SendNeighbourInfo]`
        // - `[SendNeighbourInfo, TheirKnowledge]`
        let mut events = Vec::with_capacity(2);
        let mut vote_send_neighbour_info = false;

        if !self.sections.has_key(new_key) {
            // Only vote `TheirKeyInfo` for non-neighbours. For neighbours, we update the keys
            // via `NeighbourInfo`.
            if is_neighbour {
                vote_send_neighbour_info = true;
            } else {
                events.push(AccumulatingEvent::TheirKeyInfo {
                    prefix,
                    key: *new_key,
                });
            }
        }

        if let Some(dst_key) = dst_key {
            let old = self.sections.knowledge_by_section(&prefix);
            let new = self.our_history.index_of(dst_key).unwrap_or(0);

            if new > old {
                events.push(AccumulatingEvent::TheirKnowledge {
                    prefix,
                    knowledge: new,
                })
            }

            if is_neighbour && new < self.our_history.last_key_index() {
                vote_send_neighbour_info = true;
            }
        }

        if vote_send_neighbour_info {
            // TODO: if src has split, consider sending to all child prefixes that are still our
            // neighbours.

            events.push(AccumulatingEvent::SendNeighbourInfo {
                dst: prefix.name(),
                nonce: *hash,
            })
        }

        events
    }

    // Tries to split our section.
    // If we have enough mature nodes for both subsections, returns the elders infos of the two
    // subsections. Otherwise returns `None`.
    fn try_split(
        &self,
        network_params: &NetworkParams,
        our_name: &XorName,
    ) -> Option<(EldersInfo, EldersInfo)> {
        let next_bit_index = self.our_prefix().bit_count();
        let next_bit = our_name.bit(next_bit_index);

        let (our_new_size, sibling_new_size) = self
            .our_members
            .mature()
            .map(|p2p_node| p2p_node.name().bit(next_bit_index) == next_bit)
            .fold((0, 0), |(ours, siblings), is_our_prefix| {
                if is_our_prefix {
                    (ours + 1, siblings)
                } else {
                    (ours, siblings + 1)
                }
            });

        // If either of the two new sections will not contain enough entries, return `false`.
        if our_new_size < network_params.recommended_section_size
            || sibling_new_size < network_params.recommended_section_size
        {
            return None;
        }

        let our_prefix = self.our_prefix().pushed(next_bit);
        let other_prefix = self.our_prefix().pushed(!next_bit);

        let our_elders = self
            .our_members
            .elder_candidates_matching_prefix(&our_prefix, network_params.elder_size);
        let other_elders = self
            .our_members
            .elder_candidates_matching_prefix(&other_prefix, network_params.elder_size);

        let our_info = EldersInfo::new(our_elders, our_prefix, self.our_info().version + 1);
        let other_info = EldersInfo::new(other_elders, other_prefix, self.our_info().version + 1);

        Some((our_info, other_info))
    }

    // Returns the candidates for elders out of all the nodes in the section, even out of the
    // relocating nodes if there would not be enough instead.
    fn elder_candidates(&self, elder_size: usize) -> BTreeMap<XorName, P2pNode> {
        let mut elders = self.our_members.elder_candidates(elder_size);

        // Ensure that we can still handle one node lost when relocating.
        // Ensure that the node we eject are the one we want to relocate first.
        let missing = elder_size.saturating_sub(elders.len());
        elders.extend(self.elder_candidates_from_relocating(missing));
        elders
    }

    /// Returns the `count` candidates for elders out of currently relocating nodes. Use this
    /// method when we don't have enough non-relocating nodes in the section to become elders.
    fn elder_candidates_from_relocating<'a>(
        &'a self,
        count: usize,
    ) -> impl Iterator<Item = (XorName, P2pNode)> + 'a {
        self.relocate_queue
            .iter()
            .map(|details| details.pub_id.name())
            .filter_map(move |name| self.our_members.get(name))
            .filter(|info| info.state != MemberState::Left)
            .take(count)
            .map(|info| (*info.p2p_node.name(), info.p2p_node.clone()))
    }

    // Increment the age counters of the members.
    fn increment_age_counters(&mut self, trigger_node: &XorName, recommended_section_size: usize) {
        let our_section_size = self.our_members.joined().count();
        let our_prefix = &self.sections.our().prefix;

        // Is network startup in progress?
        let startup =
            *our_prefix == Prefix::default() && our_section_size < recommended_section_size;

        // As a measure against sybil attacks, we don't increment the age counters on infant churn
        // once we completed the startup phase.
        if !startup
            && !self.our_members.is_mature(trigger_node)
            && !self.is_peer_our_elder(trigger_node)
        {
            trace!(
                "Not incrementing age counters on infant churn (section size: {})",
                our_section_size,
            );
            return;
        }

        let relocating_state = self.create_relocating_state();
        let first_key = self.our_history.first_key();

        for member_info in self.our_members.joined_mut() {
            if member_info.p2p_node.name() == trigger_node {
                continue;
            }

            // During network startup we go through accelerated ageing.
            if startup {
                member_info.increment_age();
                continue;
            }

            if !member_info.increment_age_counter() {
                continue;
            }

            let destination = relocation::compute_destination(
                our_prefix,
                member_info.p2p_node.name(),
                trigger_node,
            );
            if our_prefix.matches(&destination) {
                // Relocation destination inside the current section - ignoring.
                trace!(
                    "increment_age_counters: Ignoring relocation for {:?}",
                    member_info.p2p_node.public_id()
                );
                continue;
            }

            trace!(
                "Change state to Relocating {}",
                member_info.p2p_node.public_id()
            );
            member_info.state = relocating_state;

            let destination_key = *self.sections.key_by_name(&destination).unwrap_or(first_key);
            let details = RelocateDetails {
                pub_id: *member_info.p2p_node.public_id(),
                destination,
                destination_key,
                // TODO: why the +1 ?
                age: member_info.age() + 1,
            };

            self.relocate_queue.push_front(details);
        }

        trace!("increment_age_counters: {:?}", self.our_members);
    }

    // Return a relocating state of a node relocating now.
    // Ensure that node knows enough to trust node_knowledge proving index.
    fn create_relocating_state(&self) -> MemberState {
        let node_knowledge = self.sections.knowledge_by_section(self.our_prefix());
        MemberState::Relocating { node_knowledge }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        consensus::generate_bls_threshold_secret_key,
        id::{FullId, P2pNode, PublicId},
        rng::{self, MainRng},
        section::EldersInfo,
        xor_space::{Prefix, XorName},
    };
    use rand::{seq::SliceRandom, Rng};
    use std::{
        collections::{BTreeMap, HashMap},
        str::FromStr,
    };

    // Note: The following tests were move over from the former `chain` module.

    enum SecInfoGen<'a> {
        New(Prefix<XorName>, usize),
        Add(&'a EldersInfo),
        Remove(&'a EldersInfo),
    }

    fn gen_section_info(
        rng: &mut MainRng,
        gen: SecInfoGen,
    ) -> (EldersInfo, HashMap<PublicId, FullId>) {
        match gen {
            SecInfoGen::New(prefix, n) => {
                let mut full_ids = HashMap::new();
                let mut members = BTreeMap::new();
                for _ in 0..n {
                    let some_id = FullId::within_range(rng, &prefix.range_inclusive());
                    let peer_addr = ([127, 0, 0, 1], 9999).into();
                    let pub_id = *some_id.public_id();
                    let _ = members.insert(*pub_id.name(), P2pNode::new(pub_id, peer_addr));
                    let _ = full_ids.insert(*some_id.public_id(), some_id);
                }
                (EldersInfo::new(members, prefix, 0), full_ids)
            }
            SecInfoGen::Add(info) => {
                let mut members = info.elders.clone();
                let some_id = FullId::within_range(rng, &info.prefix.range_inclusive());
                let peer_addr = ([127, 0, 0, 1], 9999).into();
                let pub_id = *some_id.public_id();
                let _ = members.insert(*pub_id.name(), P2pNode::new(pub_id, peer_addr));
                let mut full_ids = HashMap::new();
                let _ = full_ids.insert(pub_id, some_id);
                (
                    EldersInfo::new(members, info.prefix, info.version + 1),
                    full_ids,
                )
            }
            SecInfoGen::Remove(info) => {
                let elders = info.elders.clone();
                (
                    EldersInfo::new(elders, info.prefix, info.version + 1),
                    Default::default(),
                )
            }
        }
    }

    fn add_neighbour_elders_info(
        state: &mut SharedState,
        our_id: &PublicId,
        neighbour_info: EldersInfo,
    ) {
        assert!(
            !neighbour_info.prefix.matches(our_id.name()),
            "Only add neighbours."
        );
        state.sections.add_neighbour(neighbour_info)
    }

    fn gen_state<T>(rng: &mut MainRng, sections: T) -> (SharedState, PublicId)
    where
        T: IntoIterator<Item = (Prefix<XorName>, usize)>,
    {
        let mut our_id = None;
        let mut section_members = vec![];
        for (prefix, size) in sections {
            let (info, ids) = gen_section_info(rng, SecInfoGen::New(prefix, size));
            if our_id.is_none() {
                our_id = ids.values().next().cloned();
            }

            section_members.push(info);
        }

        let our_id = our_id.expect("our id");
        let our_pub_id = *our_id.public_id();
        let mut sections_iter = section_members.into_iter();

        let elders_info = sections_iter.next().expect("section members");
        let participants = elders_info.elders.len();
        let secret_key_set = generate_bls_threshold_secret_key(rng, participants);
        let public_key = secret_key_set.public_keys().public_key();

        let mut state = SharedState::new(elders_info, public_key);

        for info in sections_iter {
            add_neighbour_elders_info(&mut state, &our_pub_id, info);
        }

        (state, our_pub_id)
    }

    fn gen_00_state(rng: &mut MainRng) -> (SharedState, PublicId) {
        let elder_size: usize = 7;
        gen_state(
            rng,
            vec![
                (Prefix::from_str("00").unwrap(), elder_size),
                (Prefix::from_str("01").unwrap(), elder_size),
                (Prefix::from_str("10").unwrap(), elder_size),
            ],
        )
    }

    fn check_infos_for_duplication(state: &SharedState) {
        let mut prefixes: Vec<Prefix<XorName>> = vec![];
        for (_, info) in state.sections.all() {
            if let Some(prefix) = prefixes.iter().find(|x| x.is_compatible(&info.prefix)) {
                panic!(
                    "Found compatible prefixes! {:?} and {:?}",
                    prefix, info.prefix
                );
            }
            prefixes.push(info.prefix);
        }
    }

    #[test]
    fn generate_state() {
        let mut rng = rng::new();

        let (state, our_id) = gen_00_state(&mut rng);

        assert_eq!(
            state
                .sections
                .get(&Prefix::from_str("00").unwrap())
                .map(|info| info.elders.contains_key(our_id.name())),
            Some(true)
        );
        assert_eq!(state.sections.get(&Prefix::from_str("").unwrap()), None);
        assert!(state.our_history.self_verify());
        check_infos_for_duplication(&state);
    }

    #[test]
    fn neighbour_info_cleaning() {
        let mut rng = rng::new();
        let (mut state, our_id) = gen_00_state(&mut rng);
        for _ in 0..100 {
            let (new_info, _) = {
                let old_info: Vec<_> = state.sections.other().map(|(_, info)| info).collect();
                let info = old_info.choose(&mut rng).expect("neighbour infos");
                if rng.gen_bool(0.5) {
                    gen_section_info(&mut rng, SecInfoGen::Add(info))
                } else {
                    gen_section_info(&mut rng, SecInfoGen::Remove(info))
                }
            };

            add_neighbour_elders_info(&mut state, &our_id, new_info);
            assert!(state.our_history.self_verify());
            check_infos_for_duplication(&state);
        }
    }
}
