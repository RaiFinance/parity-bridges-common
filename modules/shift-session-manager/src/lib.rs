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

//! Substrate session manager that selects 2/3 validators from initial set,
//! starting from session 2.

#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{decl_module, decl_storage};
use sp_std::prelude::*;

/// The module configuration trait.
pub trait Trait: pallet_session::Trait {}

decl_module! {
	/// Shift session manager pallet.
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {}
}

decl_storage! {
	trait Store for Module<T: Trait> as ShiftSessionManager {
		/// Validators of first two sessions.
		InitialValidators: Option<Vec<T::ValidatorId>>;
	}
}

impl<T: Trait> pallet_session::SessionManager<T::ValidatorId> for Module<T> {
	fn end_session(_: sp_staking::SessionIndex) {}
	fn start_session(_: sp_staking::SessionIndex) {}
	fn new_session(session_index: sp_staking::SessionIndex) -> Option<Vec<T::ValidatorId>> {
		// we don't want to add even more fields to genesis config => just return None
		if session_index == 0 || session_index == 1 {
			return None;
		}

		// the idea that on first call (i.e. when session 1 ends) we're reading current
		// set of validators from session module (they are initial validators) and save
		// in our 'local storage'.
		// then for every session we select (deterministically) 2/3 of these initial
		// validators to serve validators of new session
		let available_validators = InitialValidators::<T>::get().unwrap_or_else(|| {
			let validators = <pallet_session::Module<T>>::validators();
			InitialValidators::<T>::put(validators.clone());
			validators
		});

		Some(Self::select_validators(session_index, &available_validators))
	}
}

impl<T: Trait> Module<T> {
	/// Select validators for session.
	fn select_validators(
		session_index: sp_staking::SessionIndex,
		available_validators: &[T::ValidatorId],
	) -> Vec<T::ValidatorId> {
		let available_validators_count = available_validators.len();
		let count = sp_std::cmp::max(1, 2 * available_validators_count / 3);
		let offset = session_index as usize % available_validators_count;
		let end = offset + count;
		let session_validators = match end.overflowing_sub(available_validators_count) {
			(wrapped_end, false) if wrapped_end != 0 => available_validators[offset..]
				.iter()
				.chain(available_validators[..wrapped_end].iter())
				.cloned()
				.collect(),
			_ => available_validators[offset..end].to_vec(),
		};

		session_validators
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use frame_support::sp_io::TestExternalities;
	use frame_support::sp_runtime::{
		testing::{Header, UintAuthorityId},
		traits::{BlakeTwo256, ConvertInto, IdentityLookup},
		Perbill, RuntimeAppPublic,
	};
	use frame_support::{impl_outer_origin, parameter_types, weights::Weight};
	use sp_core::H256;

	type AccountId = u64;

	#[derive(Clone, Eq, PartialEq)]
	pub struct TestRuntime;

	impl_outer_origin! {
		pub enum Origin for TestRuntime {}
	}

	parameter_types! {
		pub const BlockHashCount: u64 = 250;
		pub const MaximumBlockWeight: Weight = 1024;
		pub const MaximumBlockLength: u32 = 2 * 1024;
		pub const AvailableBlockRatio: Perbill = Perbill::one();
	}

	impl frame_system::Trait for TestRuntime {
		type Origin = Origin;
		type Index = u64;
		type Call = ();
		type BlockNumber = u64;
		type Hash = H256;
		type Hashing = BlakeTwo256;
		type AccountId = AccountId;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type Event = ();
		type BlockHashCount = BlockHashCount;
		type MaximumBlockWeight = MaximumBlockWeight;
		type DbWeight = ();
		type BlockExecutionWeight = ();
		type ExtrinsicBaseWeight = ();
		type MaximumExtrinsicWeight = ();
		type AvailableBlockRatio = AvailableBlockRatio;
		type MaximumBlockLength = MaximumBlockLength;
		type Version = ();
		type PalletInfo = ();
		type AccountData = ();
		type OnNewAccount = ();
		type OnKilledAccount = ();
		type BaseCallFilter = ();
		type SystemWeightInfo = ();
	}

	parameter_types! {
		pub const Period: u64 = 1;
		pub const Offset: u64 = 0;
	}

	impl pallet_session::Trait for TestRuntime {
		type Event = ();
		type ValidatorId = <Self as frame_system::Trait>::AccountId;
		type ValidatorIdOf = ConvertInto;
		type ShouldEndSession = pallet_session::PeriodicSessions<Period, Offset>;
		type NextSessionRotation = pallet_session::PeriodicSessions<Period, Offset>;
		type SessionManager = ();
		type SessionHandler = TestSessionHandler;
		type Keys = UintAuthorityId;
		type DisabledValidatorsThreshold = ();
		type WeightInfo = ();
	}

	impl Trait for TestRuntime {}

	pub struct TestSessionHandler;
	impl pallet_session::SessionHandler<AccountId> for TestSessionHandler {
		const KEY_TYPE_IDS: &'static [sp_runtime::KeyTypeId] = &[UintAuthorityId::ID];

		fn on_genesis_session<Ks: sp_runtime::traits::OpaqueKeys>(_validators: &[(AccountId, Ks)]) {}

		fn on_new_session<Ks: sp_runtime::traits::OpaqueKeys>(_: bool, _: &[(AccountId, Ks)], _: &[(AccountId, Ks)]) {}

		fn on_disabled(_: usize) {}
	}

	fn new_test_ext() -> TestExternalities {
		let mut t = frame_system::GenesisConfig::default()
			.build_storage::<TestRuntime>()
			.unwrap();
		pallet_session::GenesisConfig::<TestRuntime> {
			keys: vec![
				(1, 1, UintAuthorityId(1)),
				(2, 2, UintAuthorityId(2)),
				(3, 3, UintAuthorityId(3)),
				(4, 4, UintAuthorityId(4)),
				(5, 5, UintAuthorityId(5)),
			],
		}
		.assimilate_storage(&mut t)
		.unwrap();
		TestExternalities::new(t)
	}

	#[test]
	fn shift_session_manager_works() {
		new_test_ext().execute_with(|| {
			let all_accs = vec![1, 2, 3, 4, 5];

			// at least 1 validator is selected
			assert_eq!(Module::<TestRuntime>::select_validators(0, &[1]), vec![1],);

			// at session#0, shift is also 0
			assert_eq!(Module::<TestRuntime>::select_validators(0, &all_accs), vec![1, 2, 3],);

			// at session#1, shift is also 1
			assert_eq!(Module::<TestRuntime>::select_validators(1, &all_accs), vec![2, 3, 4],);

			// at session#3, we're wrapping
			assert_eq!(Module::<TestRuntime>::select_validators(3, &all_accs), vec![4, 5, 1],);

			// at session#5, we're starting from the beginning again
			assert_eq!(Module::<TestRuntime>::select_validators(5, &all_accs), vec![1, 2, 3],);
		});
	}
}
