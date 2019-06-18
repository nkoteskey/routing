// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::Bootstrapped;
use crate::{
    chain::SectionInfo,
    error::RoutingError,
    id::PublicId,
    messages::{HopMessage, RoutingMessage, SignedRoutingMessage},
    routing_message_filter::FilteringResult,
    routing_table::Authority,
    time::Instant,
    xor_name::XorName,
};
use std::collections::BTreeSet;

pub trait BootstrappedNotEstablished: Bootstrapped {
    // Whether acknowledge hop messages sent to us.
    const SEND_ACK: bool;

    fn get_proxy_public_id(&self, proxy_name: &XorName) -> Result<&PublicId, RoutingError>;

    fn filter_hop_message(
        &mut self,
        hop_msg: HopMessage,
    ) -> Result<Option<RoutingMessage>, RoutingError> {
        let signed_msg = hop_msg.content;
        let routing_msg = signed_msg.into_routing_message();
        let in_authority = self.in_authority(&routing_msg.dst);
        if in_authority && Self::SEND_ACK {
            self.send_ack(&routing_msg, 0);
        }

        // Prevents us repeatedly handling identical messages sent by a malicious peer.
        match self
            .routing_msg_filter()
            .filter_incoming(&routing_msg, hop_msg.route)
        {
            FilteringResult::KnownMessage | FilteringResult::KnownMessageAndRoute => {
                return Err(RoutingError::FilterCheckFailed);
            }
            FilteringResult::NewMessage => (),
        }

        if !in_authority {
            return Ok(None);
        }

        Ok(Some(routing_msg))
    }

    fn send_routing_message_via_proxy(
        &mut self,
        routing_msg: RoutingMessage,
        src_section: Option<SectionInfo>,
        route: u8,
        expires_at: Option<Instant>,
    ) -> Result<(), RoutingError> {
        if routing_msg.dst.is_client() && self.in_authority(&routing_msg.dst) {
            return Ok(()); // Message is for us.
        }

        // Get PublicId of the proxy node
        let proxy_pub_id = match routing_msg.src {
            Authority::Client {
                ref client_id,
                ref proxy_node_name,
            } => {
                if self.name() != client_id.name() {
                    return Ok(());
                }

                *self.get_proxy_public_id(proxy_node_name)?
            }
            _ => {
                error!("{} - Source should be client in this state", self);
                return Err(RoutingError::InvalidSource);
            }
        };

        let signed_msg = SignedRoutingMessage::new(routing_msg, self.full_id(), None)?;

        if self.add_to_pending_acks(signed_msg.routing_message(), src_section, route, expires_at)
            && !self.filter_outgoing_routing_msg(signed_msg.routing_message(), &proxy_pub_id, route)
        {
            let message = self.to_hop_message(signed_msg.clone(), route, BTreeSet::new())?;
            self.send_message(&proxy_pub_id, message);
        }

        Ok(())
    }
}
