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

//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.

// =====================================================================================
// =====================================================================================
// =====================================================================================
// UPDATE GUIDE:
// 1) replace everything with node-template/src/service.rs contents (found in main Substrate repo);
// 2) the only thing to keep from old code, is `rpc_extensions_builder` - we use our own custom RPCs;
// 3) fix compilation errors;
// 4) test :)
// =====================================================================================
// =====================================================================================
// =====================================================================================

use bp_message_lane::{LaneId, MessageNonce};
use bp_runtime::{InstanceId, MILLAU_BRIDGE_INSTANCE};
use rialto_runtime::{self, opaque::Block, RuntimeApi};
use sc_client_api::{ExecutorProvider, RemoteBackend};
use sc_executor::native_executor_instance;
pub use sc_executor::NativeExecutor;
use sc_finality_grandpa::{FinalityProofProvider as GrandpaFinalityProofProvider, SharedVoterState};
use sc_service::{error::Error as ServiceError, Configuration, TaskManager};
use sp_consensus_aura::sr25519::AuthorityPair as AuraPair;
use sp_core::storage::StorageKey;
use sp_inherents::InherentDataProviders;
use std::sync::Arc;
use std::time::Duration;

// Our native executor instance.
native_executor_instance!(
	pub Executor,
	rialto_runtime::api::dispatch,
	rialto_runtime::native_version,
	frame_benchmarking::benchmarking::HostFunctions,
);

type FullClient = sc_service::TFullClient<Block, RuntimeApi, Executor>;
type FullBackend = sc_service::TFullBackend<Block>;
type FullSelectChain = sc_consensus::LongestChain<FullBackend, Block>;

#[allow(clippy::type_complexity)]
pub fn new_partial(
	config: &Configuration,
) -> Result<
	sc_service::PartialComponents<
		FullClient,
		FullBackend,
		FullSelectChain,
		sp_consensus::DefaultImportQueue<Block, FullClient>,
		sc_transaction_pool::FullPool<Block, FullClient>,
		(
			sc_finality_grandpa::GrandpaBlockImport<FullBackend, Block, FullClient, FullSelectChain>,
			sc_finality_grandpa::LinkHalf<Block, FullClient, FullSelectChain>,
		),
	>,
	ServiceError,
> {
	let inherent_data_providers = sp_inherents::InherentDataProviders::new();

	let (client, backend, keystore, task_manager) = sc_service::new_full_parts::<Block, RuntimeApi, Executor>(&config)?;
	let client = Arc::new(client);

	let select_chain = sc_consensus::LongestChain::new(backend.clone());

	let transaction_pool = sc_transaction_pool::BasicPool::new_full(
		config.transaction_pool.clone(),
		config.prometheus_registry(),
		task_manager.spawn_handle(),
		client.clone(),
	);

	let (grandpa_block_import, grandpa_link) =
		sc_finality_grandpa::block_import(client.clone(), &(client.clone() as Arc<_>), select_chain.clone())?;

	let aura_block_import =
		sc_consensus_aura::AuraBlockImport::<_, _, _, AuraPair>::new(grandpa_block_import.clone(), client.clone());

	let import_queue = sc_consensus_aura::import_queue::<_, _, _, AuraPair, _, _>(
		sc_consensus_aura::slot_duration(&*client)?,
		aura_block_import,
		Some(Box::new(grandpa_block_import.clone())),
		None,
		client.clone(),
		inherent_data_providers.clone(),
		&task_manager.spawn_handle(),
		config.prometheus_registry(),
		sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone()),
	)?;

	Ok(sc_service::PartialComponents {
		client,
		backend,
		task_manager,
		import_queue,
		keystore,
		select_chain,
		transaction_pool,
		inherent_data_providers,
		other: (grandpa_block_import, grandpa_link),
	})
}

/// Builds a new service for a full client.
pub fn new_full(config: Configuration) -> Result<TaskManager, ServiceError> {
	let sc_service::PartialComponents {
		client,
		backend,
		mut task_manager,
		import_queue,
		keystore,
		select_chain,
		transaction_pool,
		inherent_data_providers,
		other: (block_import, grandpa_link),
	} = new_partial(&config)?;

	let finality_proof_provider = GrandpaFinalityProofProvider::new_for_service(backend.clone(), client.clone());

	let (network, network_status_sinks, system_rpc_tx, network_starter) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue,
			on_demand: None,
			block_announce_validator_builder: None,
			finality_proof_request_builder: None,
			finality_proof_provider: Some(finality_proof_provider),
		})?;

	if config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&config,
			backend.clone(),
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	let role = config.role.clone();
	let force_authoring = config.force_authoring;
	let name = config.network.node_name.clone();
	let enable_grandpa = !config.disable_grandpa;
	let prometheus_registry = config.prometheus_registry().cloned();
	let telemetry_connection_sinks = sc_service::TelemetryConnectionSinks::default();

	let rpc_extensions_builder = {
		// This struct is here to ease update process.

		/// Rialto runtime from message-lane RPC point of view.
		struct RialtoMessageLaneKeys;

		impl pallet_message_lane_rpc::Runtime for RialtoMessageLaneKeys {
			fn message_key(&self, instance: &InstanceId, lane: &LaneId, nonce: MessageNonce) -> Option<StorageKey> {
				match *instance {
					MILLAU_BRIDGE_INSTANCE => Some(rialto_runtime::millau_messages::message_key(lane, nonce)),
					_ => None,
				}
			}

			fn outbound_lane_data_key(&self, instance: &InstanceId, lane: &LaneId) -> Option<StorageKey> {
				match *instance {
					MILLAU_BRIDGE_INSTANCE => Some(rialto_runtime::millau_messages::outbound_lane_data_key(lane)),
					_ => None,
				}
			}

			fn inbound_lane_data_key(&self, instance: &InstanceId, lane: &LaneId) -> Option<StorageKey> {
				match *instance {
					MILLAU_BRIDGE_INSTANCE => Some(rialto_runtime::millau_messages::inbound_lane_data_key(lane)),
					_ => None,
				}
			}
		}

		use pallet_message_lane_rpc::{MessageLaneApi, MessageLaneRpcHandler};
		use sc_finality_grandpa_rpc::{GrandpaApi, GrandpaRpcHandler};
		use sc_rpc::DenyUnsafe;
		use substrate_frame_rpc_system::{FullSystem, SystemApi};
		let backend = backend.clone();
		let client = client.clone();
		let pool = transaction_pool.clone();

		let justification_stream = grandpa_link.justification_stream();
		let shared_authority_set = grandpa_link.shared_authority_set().clone();
		let finality_proof_provider = GrandpaFinalityProofProvider::new_for_service(backend.clone(), client.clone());

		Box::new(move |_, subscription_executor| {
			let shared_voter_state = SharedVoterState::empty();

			let mut io = jsonrpc_core::IoHandler::default();
			io.extend_with(SystemApi::to_delegate(FullSystem::new(
				client.clone(),
				pool.clone(),
				DenyUnsafe::No,
			)));
			io.extend_with(GrandpaApi::to_delegate(GrandpaRpcHandler::new(
				shared_authority_set.clone(),
				shared_voter_state,
				justification_stream.clone(),
				subscription_executor,
				finality_proof_provider.clone(),
			)));
			io.extend_with(MessageLaneApi::to_delegate(MessageLaneRpcHandler::new(
				backend.clone(),
				Arc::new(RialtoMessageLaneKeys),
			)));

			io
		})
	};

	sc_service::spawn_tasks(sc_service::SpawnTasksParams {
		network: network.clone(),
		client: client.clone(),
		keystore: keystore.clone(),
		task_manager: &mut task_manager,
		transaction_pool: transaction_pool.clone(),
		telemetry_connection_sinks: telemetry_connection_sinks.clone(),
		rpc_extensions_builder,
		on_demand: None,
		remote_blockchain: None,
		backend,
		network_status_sinks,
		system_rpc_tx,
		config,
	})?;

	if role.is_authority() {
		let proposer =
			sc_basic_authorship::ProposerFactory::new(client.clone(), transaction_pool, prometheus_registry.as_ref());

		let can_author_with = sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone());

		let aura = sc_consensus_aura::start_aura::<_, _, _, _, _, AuraPair, _, _, _>(
			sc_consensus_aura::slot_duration(&*client)?,
			client.clone(),
			select_chain,
			block_import,
			proposer,
			network.clone(),
			inherent_data_providers.clone(),
			force_authoring,
			keystore.clone(),
			can_author_with,
		)?;

		// the AURA authoring task is considered essential, i.e. if it
		// fails we take down the service with it.
		task_manager.spawn_essential_handle().spawn_blocking("aura", aura);
	}

	// if the node isn't actively participating in consensus then it doesn't
	// need a keystore, regardless of which protocol we use below.
	let keystore = if role.is_authority() {
		Some(keystore as sp_core::traits::BareCryptoStorePtr)
	} else {
		None
	};

	let grandpa_config = sc_finality_grandpa::Config {
		// FIXME #1578 make this available through chainspec
		gossip_duration: Duration::from_millis(333),
		justification_period: 512,
		name: Some(name),
		observer_enabled: false,
		keystore,
		is_authority: role.is_network_authority(),
	};

	if enable_grandpa {
		// start the full GRANDPA voter
		// NOTE: non-authorities could run the GRANDPA observer protocol, but at
		// this point the full voter should provide better guarantees of block
		// and vote data availability than the observer. The observer has not
		// been tested extensively yet and having most nodes in a network run it
		// could lead to finality stalls.
		let grandpa_config = sc_finality_grandpa::GrandpaParams {
			config: grandpa_config,
			link: grandpa_link,
			network,
			inherent_data_providers,
			telemetry_on_connect: Some(telemetry_connection_sinks.on_connect_stream()),
			voting_rule: sc_finality_grandpa::VotingRulesBuilder::default().build(),
			prometheus_registry,
			shared_voter_state: SharedVoterState::empty(),
		};

		// the GRANDPA voter task is considered infallible, i.e.
		// if it fails we take down the service with it.
		task_manager
			.spawn_essential_handle()
			.spawn_blocking("grandpa-voter", sc_finality_grandpa::run_grandpa_voter(grandpa_config)?);
	} else {
		sc_finality_grandpa::setup_disabled_grandpa(client, &inherent_data_providers, network)?;
	}

	network_starter.start_network();
	Ok(task_manager)
}

/// Builds a new service for a light client.
pub fn new_light(config: Configuration) -> Result<TaskManager, ServiceError> {
	let (client, backend, keystore, mut task_manager, on_demand) =
		sc_service::new_light_parts::<Block, RuntimeApi, Executor>(&config)?;

	let transaction_pool = Arc::new(sc_transaction_pool::BasicPool::new_light(
		config.transaction_pool.clone(),
		config.prometheus_registry(),
		task_manager.spawn_handle(),
		client.clone(),
		on_demand.clone(),
	));

	let grandpa_block_import = sc_finality_grandpa::light_block_import(
		client.clone(),
		backend.clone(),
		&(client.clone() as Arc<_>),
		Arc::new(on_demand.checker().clone()) as Arc<_>,
	)?;
	let finality_proof_import = grandpa_block_import.clone();
	let finality_proof_request_builder = finality_proof_import.create_finality_proof_request_builder();

	let import_queue = sc_consensus_aura::import_queue::<_, _, _, AuraPair, _, _>(
		sc_consensus_aura::slot_duration(&*client)?,
		grandpa_block_import,
		None,
		Some(Box::new(finality_proof_import)),
		client.clone(),
		InherentDataProviders::new(),
		&task_manager.spawn_handle(),
		config.prometheus_registry(),
		sp_consensus::NeverCanAuthor,
	)?;

	let finality_proof_provider = GrandpaFinalityProofProvider::new_for_service(backend.clone(), client.clone());

	let (network, network_status_sinks, system_rpc_tx, network_starter) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue,
			on_demand: Some(on_demand.clone()),
			block_announce_validator_builder: None,
			finality_proof_request_builder: Some(finality_proof_request_builder),
			finality_proof_provider: Some(finality_proof_provider),
		})?;

	if config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&config,
			backend.clone(),
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	sc_service::spawn_tasks(sc_service::SpawnTasksParams {
		remote_blockchain: Some(backend.remote_blockchain()),
		transaction_pool,
		task_manager: &mut task_manager,
		on_demand: Some(on_demand),
		rpc_extensions_builder: Box::new(|_, _| ()),
		telemetry_connection_sinks: sc_service::TelemetryConnectionSinks::default(),
		config,
		client,
		keystore,
		backend,
		network,
		network_status_sinks,
		system_rpc_tx,
	})?;

	network_starter.start_network();

	Ok(task_manager)
}
