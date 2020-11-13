// Copyright 2019-2020 Parity Technologies (UK) Ltd.
// This file is part of Parity Bridges Common.

// Parity Bridges Common is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Bridges Common is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Bridges Common.  If not, see <http://www.gnu.org/licenses/>.

//! Substrate client as Substrate messages source. The chain we connect to should have
//! runtime that implements `<BridgedChainName>HeaderApi` to allow bridging with
//! <BridgedName> chain.

use crate::messages_lane::SubstrateMessageLane;

use async_trait::async_trait;
use bp_message_lane::{LaneId, MessageNonce};
use bp_runtime::InstanceId;
use codec::{Decode, Encode};
use frame_support::weights::Weight;
use messages_relay::{
	message_lane::{SourceHeaderIdOf, TargetHeaderIdOf},
	message_lane_loop::{ClientState, MessageProofParameters, MessageWeightsMap, SourceClient, SourceClientState},
};
use relay_substrate_client::{Chain, Client, Error as SubstrateError, HashOf, HeaderIdOf};
use relay_utils::HeaderId;
use sp_core::Bytes;
use sp_runtime::{traits::Header as HeaderT, DeserializeOwned};
use sp_trie::StorageProof;
use std::ops::RangeInclusive;

/// Intermediate message proof returned by the source Substrate node. Includes everything
/// required to submit to the target node: cumulative dispatch weight of bundled messages and
/// the proof itself.
pub type SubstrateMessagesProof<C> = (Weight, (HashOf<C>, StorageProof, LaneId, MessageNonce, MessageNonce));

/// Substrate client as Substrate messages source.
pub struct SubstrateMessagesSource<C: Chain, P> {
	client: Client<C>,
	lane: P,
	lane_id: LaneId,
	instance: InstanceId,
}

impl<C: Chain, P> SubstrateMessagesSource<C, P> {
	/// Create new Substrate headers source.
	pub fn new(client: Client<C>, lane: P, lane_id: LaneId, instance: InstanceId) -> Self {
		SubstrateMessagesSource {
			client,
			lane,
			lane_id,
			instance,
		}
	}
}

impl<C: Chain, P: SubstrateMessageLane> Clone for SubstrateMessagesSource<C, P> {
	fn clone(&self) -> Self {
		Self {
			client: self.client.clone(),
			lane: self.lane.clone(),
			lane_id: self.lane_id,
			instance: self.instance,
		}
	}
}

#[async_trait]
impl<C, P> SourceClient<P> for SubstrateMessagesSource<C, P>
where
	C: Chain,
	C::Header: DeserializeOwned,
	C::Index: DeserializeOwned,
	<C::Header as HeaderT>::Number: Into<u64>,
	P: SubstrateMessageLane<
		MessagesProof = SubstrateMessagesProof<C>,
		SourceHeaderNumber = <C::Header as HeaderT>::Number,
		SourceHeaderHash = <C::Header as HeaderT>::Hash,
	>,
	P::TargetHeaderNumber: Decode,
	P::TargetHeaderHash: Decode,
{
	type Error = SubstrateError;

	async fn reconnect(mut self) -> Result<Self, Self::Error> {
		let new_client = self.client.clone().reconnect().await?;
		self.client = new_client;
		Ok(self)
	}

	async fn state(&self) -> Result<SourceClientState<P>, Self::Error> {
		read_client_state::<_, P::TargetHeaderHash, P::TargetHeaderNumber>(
			&self.client,
			P::BEST_FINALIZED_TARGET_HEADER_ID_AT_SOURCE,
		)
		.await
	}

	async fn latest_generated_nonce(
		&self,
		id: SourceHeaderIdOf<P>,
	) -> Result<(SourceHeaderIdOf<P>, MessageNonce), Self::Error> {
		let encoded_response = self
			.client
			.state_call(
				P::OUTBOUND_LANE_LATEST_GENERATED_NONCE_METHOD.into(),
				Bytes(self.lane_id.encode()),
				Some(id.1),
			)
			.await?;
		let latest_generated_nonce: MessageNonce =
			Decode::decode(&mut &encoded_response.0[..]).map_err(SubstrateError::ResponseParseFailed)?;
		Ok((id, latest_generated_nonce))
	}

	async fn latest_confirmed_received_nonce(
		&self,
		id: SourceHeaderIdOf<P>,
	) -> Result<(SourceHeaderIdOf<P>, MessageNonce), Self::Error> {
		let encoded_response = self
			.client
			.state_call(
				P::OUTBOUND_LANE_LATEST_RECEIVED_NONCE_METHOD.into(),
				Bytes(self.lane_id.encode()),
				Some(id.1),
			)
			.await?;
		let latest_received_nonce: MessageNonce =
			Decode::decode(&mut &encoded_response.0[..]).map_err(SubstrateError::ResponseParseFailed)?;
		Ok((id, latest_received_nonce))
	}

	async fn generated_messages_weights(
		&self,
		id: SourceHeaderIdOf<P>,
		nonces: RangeInclusive<MessageNonce>,
	) -> Result<MessageWeightsMap, Self::Error> {
		let encoded_response = self
			.client
			.state_call(
				P::OUTBOUND_LANE_MESSAGES_DISPATCH_WEIGHT_METHOD.into(),
				Bytes((self.lane_id, nonces.start(), nonces.end()).encode()),
				Some(id.1),
			)
			.await?;
		let weights: Vec<(MessageNonce, Weight)> =
			Decode::decode(&mut &encoded_response.0[..]).map_err(SubstrateError::ResponseParseFailed)?;

		let mut expected_nonce = *nonces.start();
		let mut weights_map = MessageWeightsMap::new();
		for (nonce, weight) in weights {
			if nonce != expected_nonce {
				return Err(SubstrateError::Custom(format!(
					"Unexpected nonce in messages_dispatch_weight call result. Expected {}, got {}",
					expected_nonce, nonce
				)));
			}

			weights_map.insert(nonce, weight);
			expected_nonce += 1;
		}
		Ok(weights_map)
	}

	async fn prove_messages(
		&self,
		id: SourceHeaderIdOf<P>,
		nonces: RangeInclusive<MessageNonce>,
		proof_parameters: MessageProofParameters,
	) -> Result<(SourceHeaderIdOf<P>, RangeInclusive<MessageNonce>, P::MessagesProof), Self::Error> {
		let proof = self
			.client
			.prove_messages(
				self.instance,
				self.lane_id,
				nonces.clone(),
				proof_parameters.outbound_state_proof_required,
				id.1,
			)
			.await?;
		let proof = (id.1, proof, self.lane_id, *nonces.start(), *nonces.end());
		Ok((id, nonces, (proof_parameters.dispatch_weight, proof)))
	}

	async fn submit_messages_receiving_proof(
		&self,
		generated_at_block: TargetHeaderIdOf<P>,
		proof: P::MessagesReceivingProof,
	) -> Result<(), Self::Error> {
		let tx = self
			.lane
			.make_messages_receiving_proof_transaction(generated_at_block, proof)
			.await?;
		self.client.submit_extrinsic(Bytes(tx.encode())).await?;
		Ok(())
	}
}

pub async fn read_client_state<SelfChain, BridgedHeaderHash, BridgedHeaderNumber>(
	self_client: &Client<SelfChain>,
	best_finalized_header_id_method_name: &str,
) -> Result<ClientState<HeaderIdOf<SelfChain>, HeaderId<BridgedHeaderHash, BridgedHeaderNumber>>, SubstrateError>
where
	SelfChain: Chain,
	SelfChain::Header: DeserializeOwned,
	SelfChain::Index: DeserializeOwned,
	BridgedHeaderHash: Decode,
	BridgedHeaderNumber: Decode,
{
	// let's read our state first: we need best finalized header hash on **this** chain
	let self_best_finalized_header_hash = self_client.best_finalized_header_hash().await?;
	let self_best_finalized_header = self_client.header_by_hash(self_best_finalized_header_hash).await?;
	let self_best_finalized_id = HeaderId(*self_best_finalized_header.number(), self_best_finalized_header_hash);

	// now let's read id of best finalized peer header at our best finalized block
	let encoded_best_finalized_peer_on_self = self_client
		.state_call(
			best_finalized_header_id_method_name.into(),
			Bytes(Vec::new()),
			Some(self_best_finalized_header_hash),
		)
		.await?;
	let decoded_best_finalized_peer_on_self: (BridgedHeaderNumber, BridgedHeaderHash) =
		Decode::decode(&mut &encoded_best_finalized_peer_on_self.0[..]).map_err(SubstrateError::ResponseParseFailed)?;
	let peer_on_self_best_finalized_id = HeaderId(
		decoded_best_finalized_peer_on_self.0,
		decoded_best_finalized_peer_on_self.1,
	);

	Ok(ClientState {
		best_self: self_best_finalized_id,
		best_peer: peer_on_self_best_finalized_id,
	})
}
