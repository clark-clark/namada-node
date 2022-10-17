//! Code for handling
//! [`namada::types::transaction::protocol::ProtocolTxType::EthereumEvents`]
//! transactions.
mod eth_msgs;
mod events;

use std::collections::{BTreeSet, HashMap, HashSet};

use eth_msgs::{EthMsg, EthMsgUpdate};
use eyre::Result;
use namada::ledger::eth_bridge::storage::vote_tracked;
use namada::ledger::storage::{DBIter, Storage, StorageHasher, DB};
use namada::types::address::Address;
use namada::types::storage::{self, BlockHeight};
use namada::types::transaction::TxResult;
use namada::types::vote_extensions::ethereum_events::MultiSignedEthEvent;
use namada::types::voting_power::FractionalVotingPower;

use crate::node::ledger::protocol::transactions::utils::{
    self, get_active_validators,
};
use crate::node::ledger::protocol::transactions::votes::{
    calculate_new, calculate_updated, write,
};

/// The keys changed while applying a protocol transaction
type ChangedKeys = BTreeSet<storage::Key>;

/// Applies derived state changes to storage, based on Ethereum `events` which
/// were newly seen by some active validator(s) in the last epoch. For `events`
/// which have been seen by enough voting power, extra state changes may take
/// place, such as minting of wrapped ERC20s.
///
/// This function is deterministic based on some existing blockchain state and
/// the passed `events`.
pub(crate) fn apply_derived_tx<D, H>(
    storage: &mut Storage<D, H>,
    events: Vec<MultiSignedEthEvent>,
) -> Result<TxResult>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    if events.is_empty() {
        return Ok(TxResult::default());
    }
    tracing::info!(
        ethereum_events = events.len(),
        "Applying state updates derived from Ethereum events found in \
         protocol transaction"
    );

    let voting_powers = get_voting_powers(storage, &events)?;

    let updates = events.into_iter().map(Into::<EthMsgUpdate>::into).collect();

    let changed_keys = apply_updates(storage, updates, voting_powers)?;

    Ok(TxResult {
        changed_keys,
        ..Default::default()
    })
}

/// Constructs a map of all validators who voted for an event to their
/// fractional voting power for block heights at which they voted for an event
fn get_voting_powers<D, H>(
    storage: &Storage<D, H>,
    events: &[MultiSignedEthEvent],
) -> Result<HashMap<(Address, BlockHeight), FractionalVotingPower>>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let voters = utils::get_votes_for_events(events.iter());
    tracing::debug!(?voters, "Got validators who voted on at least one event");

    let active_validators = get_active_validators(
        storage,
        voters.iter().map(|(_, h)| h.to_owned()).collect(),
    );
    tracing::debug!(
        n = active_validators.len(),
        "got active validators - {:#?}",
        active_validators,
    );

    let voting_powers =
        utils::get_voting_powers_for_selected(&active_validators, voters)?;
    tracing::debug!(
        ?voting_powers,
        "got voting powers for relevant validators"
    );

    Ok(voting_powers)
}

/// Apply an Ethereum state update + act on any events which are confirmed
pub(super) fn apply_updates<D, H>(
    storage: &mut Storage<D, H>,
    updates: HashSet<EthMsgUpdate>,
    voting_powers: HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<ChangedKeys>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    tracing::debug!(
        updates.len = updates.len(),
        ?voting_powers,
        "Applying Ethereum state update transaction"
    );

    let mut changed_keys = BTreeSet::default();
    let mut confirmed = vec![];
    for update in updates {
        // The order in which updates are applied to storage does not matter.
        // The final storage state will be the same regardless.
        let (mut changed, newly_confirmed) =
            apply_update(storage, update.clone(), &voting_powers)?;
        changed_keys.append(&mut changed);
        if newly_confirmed {
            confirmed.push(update.body);
        }
    }
    if confirmed.is_empty() {
        tracing::debug!("No events were newly confirmed");
        return Ok(changed_keys);
    }
    tracing::debug!(n = confirmed.len(), "Events were newly confirmed",);

    // Right now, the order in which events are acted on does not matter.
    // For `TransfersToNamada` events, they can happen in any order.
    for event in &confirmed {
        let mut changed = events::act_on(storage, event)?;
        changed_keys.append(&mut changed);
    }
    Ok(changed_keys)
}

/// Apply an [`EthMsgUpdate`] to storage. Returns any keys changed and whether
/// the event was newly seen.
fn apply_update<D, H>(
    storage: &mut Storage<D, H>,
    update: EthMsgUpdate,
    voting_powers: &HashMap<(Address, BlockHeight), FractionalVotingPower>,
) -> Result<(ChangedKeys, bool)>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    let eth_msg_keys = vote_tracked::Keys::from(&update.body);

    // we arbitrarily look at whether the seen key is present to
    // determine if the /eth_msg already exists in storage, but maybe there
    // is a less arbitrary way to do this
    let (exists_in_storage, _) = storage.has_key(&eth_msg_keys.seen())?;

    let (vote_tracking, changed, confirmed) = if !exists_in_storage {
        tracing::debug!(%eth_msg_keys.prefix, "Ethereum event not seen before by any validator");
        let vote_tracking = calculate_new(&update.seen_by, voting_powers)?;
        let changed = eth_msg_keys.into_iter().collect();
        let confirmed = vote_tracking.seen;
        (vote_tracking, changed, confirmed)
    } else {
        tracing::debug!(
            %eth_msg_keys.prefix,
            "Ethereum event already exists in storage",
        );
        let vote_tracking =
            calculate_updated(storage, &eth_msg_keys, voting_powers)?;
        let changed = BTreeSet::default(); // TODO(namada#515): calculate changed keys
        let confirmed =
            vote_tracking.seen && changed.contains(&eth_msg_keys.seen());
        (vote_tracking, changed, confirmed)
    };
    tracing::debug!("Read EthMsg - {:#?}", &eth_msg_pre);

    let mut votes = HashMap::default();
    update.seen_by.iter().for_each(|(address, block_height)| {
        let fvp = voting_powers
            .get(&(address.to_owned(), block_height.to_owned()))
            .unwrap();
        if let Some(already_present_fvp) =
            votes.insert(address.to_owned(), fvp.to_owned())
        {
            tracing::warn!(
                ?address,
                ?already_present_fvp,
                new_fvp = ?fvp,
                "Validator voted more than once, arbitrarily using later value",
            )
        }
    });

    let eth_msg_post = calculate_update(&eth_msg_pre, &votes);

    let changed_keys = validate_update(&eth_msg_pre, &eth_msg_post)
        .expect("We should always be applying a valid update");

    Ok((eth_msg_post, changed_keys))
}

/// Takes an existing [`EthMsg`] and calculates the new [`EthMsg`] based on new
/// validators which have seen it. `voting_powers` should map validators who
/// have newly seen the event to their fractional voting power at a block height
/// at which they saw the event.
fn calculate_update(
    eth_msg_pre: &EthMsg,
    votes: &HashMap<Address, FractionalVotingPower>,
) -> EthMsg {
    // TODO: refactor so that we don't need to accept the body in the first
    // place, which we just end up cloning to return
    let event = &eth_msg_pre.body;
    let event_hash = event.hash().unwrap();
    let voters: BTreeSet<Address> = votes.keys().cloned().collect();

    // For any event and validator, only the first vote by that validator for
    // that event counts, later votes we encounter here can just be ignored. We
    // can warn here when we encounter duplicate votes but these are
    // reasonably likely to occur so this perhaps shouldn't be a warning unless
    // it is happening a lot
    for validator in eth_msg_pre.seen_by.intersection(&voters) {
        tracing::warn!(
            ?event_hash,
            ?validator,
            "Encountered duplicate vote for an event by a validator, ignoring"
        );
    }
    let mut eth_msg_post_voting_power = eth_msg_pre.voting_power.clone();
    let mut eth_msg_post_seen_by = eth_msg_pre.seen_by.clone();
    for validator in voters.difference(&eth_msg_pre.seen_by) {
        tracing::info!(
            ?event_hash,
            ?validator,
            "Recording validator as having voted for this event"
        );
        eth_msg_post_seen_by.insert(validator.to_owned());
        eth_msg_post_voting_power += votes.get(validator).expect(
            "voting powers map must have all validators from newly_seen_by",
        );
    }

    let eth_msg_post_seen =
        if eth_msg_post_voting_power > FractionalVotingPower::TWO_THIRDS {
            tracing::info!(
                ?event_hash,
                "Event has been seen by a quorum of validators"
            );
            true
        } else {
            tracing::debug!(
                ?event_hash,
                "Event is not yet seen by a quorum of validators"
            );
            false
        };

    EthMsg {
        body: event.clone(),
        voting_power: eth_msg_post_voting_power,
        seen_by: eth_msg_post_seen_by,
        seen: eth_msg_post_seen,
    }
}

/// Validates that `post` is an updated version of `pre`, and returns keys which
/// changed. This function serves as a sort of validity predicate for this
/// native transaction, which is otherwise not checked by anything else.
fn validate_update(pre: &EthMsg, post: &EthMsg) -> Result<ChangedKeys> {
    // TODO: refactor, this should never be the case
    if pre.body != post.body {
        return Err(eyre!(
            "EthMsg body changed from {:#?} to {:#?}",
            &pre.body,
            &post.body,
        ));
    }

    let mut keys_changed = ChangedKeys::default();
    let keys = Keys::from(&pre.body);

    let mut seen = false;
    if pre.seen != post.seen {
        // the only valid transition for `seen` is from `false` to `true`
        if pre.seen == true || post.seen == false {
            return Err(eyre!(
                "EthMsg seen changed from {:#?} to {:#?}",
                &pre.seen,
                &post.seen,
            ));
        }
        keys_changed.insert(keys.seen());
        seen = true;
    }

    if pre.seen_by != post.seen_by {
        // if seen_by changes, it must be a strict superset of the previous
        // seen_by
        if !post.seen_by.is_superset(&pre.seen_by) {
            return Err(eyre!(
                "EthMsg seen changed from {:#?} to {:#?}",
                &pre.seen_by,
                &post.seen_by,
            ));
        }
        keys_changed.insert(keys.seen_by());
    }

    if pre.voting_power != post.voting_power {
        // if voting_power changes, it must have increased
        if pre.voting_power >= post.voting_power {
            return Err(eyre!(
                "EthMsg voting_power changed from {:#?} to {:#?}",
                &pre.voting_power,
                &post.voting_power,
            ));
        }
        keys_changed.insert(keys.voting_power());
    }

    if post.voting_power > FractionalVotingPower::TWO_THIRDS && !seen {
        if pre.voting_power >= post.voting_power {
            return Err(eyre!(
                "EthMsg is not seen even though new voting_power is enough: \
                 {:#?}",
                &post.voting_power,
            ));
        }
    }

    Ok(keys_changed)
}

fn write_eth_msg<D, H>(
    storage: &mut Storage<D, H>,
    eth_msg_keys: &Keys,
    eth_msg: &EthMsg,
) -> Result<()>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    tracing::debug!("writing EthMsg - {:#?}", eth_msg);
    storage.write(&eth_msg_keys.body(), &eth_msg.body.try_to_vec()?)?;
    storage.write(&eth_msg_keys.seen(), &eth_msg.seen.try_to_vec()?)?;
    storage.write(&eth_msg_keys.seen_by(), &eth_msg.seen_by.try_to_vec()?)?;
    storage.write(
        &eth_msg_keys.voting_power(),
        &eth_msg.voting_power.try_to_vec()?,
    )?;
    Ok((changed, confirmed))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap, HashSet};

    use borsh::BorshDeserialize;
    use namada::ledger::eth_bridge::storage::wrapped_erc20s;
    use namada::ledger::pos::namada_proof_of_stake::epoched::Epoched;
    use namada::ledger::pos::namada_proof_of_stake::PosBase;
    use namada::ledger::pos::types::{ValidatorSet, VotingPower};
    use namada::ledger::storage::mockdb::MockDB;
    use namada::ledger::storage::testing::TestStorage;
    use namada::types::address;
    use namada::types::ethereum_events::testing::{
        arbitrary_amount, arbitrary_eth_address, DAI_ERC20_ETH_ADDRESS,
    };
    use namada::types::ethereum_events::{EthereumEvent, TransferToNamada};
    use namada::types::token::Amount;
    use storage::BlockHeight;

    use super::*;

    mod helpers {
        use super::*;

        /// Wraps a [`TestStorage`] along with the addresses of validators who
        /// were set up for it
        pub struct TestSetup<const N: usize> {
            pub storage: TestStorage,
            pub genesis_validators: [Address; N],
        }

        impl<const N: usize> TestSetup<N> {
            /// Sets up a [`TestStorage`] and `N` validators with the given
            /// voting powers
            pub fn with_genesis_validator_voting_powers(
                voting_powers: [VotingPower; N],
            ) -> Self {
                let validators: Vec<_> = (0..N)
                    .map(|_| address::testing::gen_established_address())
                    .collect();
                let storage = {
                    let mut storage = TestStorage::default();
                    let validator_set = ValidatorSet {
                        active: validators
                            .iter()
                            .enumerate()
                            .map(|(i, address)| WeightedValidator {
                                voting_power: voting_powers[i],
                                address: address.to_owned(),
                            })
                            .collect(),
                        inactive: BTreeSet::default(),
                    };
                    let validator_sets =
                        Epoched::init_at_genesis(validator_set, 1);
                    storage.write_validator_set(&validator_sets);
                    storage
                };
                TestSetup {
                    storage,
                    genesis_validators: validators.try_into().unwrap(),
                }
            }
        }

        /// Models a simplified version of the Ethereum bridge smart contracts
        /// that emits events and keeps track of the nonces
        #[derive(Debug, Default)]
        pub struct FakeEthereumBridge {
            // The nonce of the most recent event emitted by the bridge
            // contract
            bridge_nonce: Option<u64>,
        }

        impl FakeEthereumBridge {
            fn emit_next_bridge_nonce(&mut self) -> u64 {
                let next = match self.bridge_nonce {
                    Some(nonce) => nonce + 1,
                    None => 0,
                };
                self.bridge_nonce = Some(next);
                next
            }

            pub fn emit_transfers_to_namada(
                &mut self,
                transfers: Vec<TransferToNamada>,
            ) -> EthereumEvent {
                EthereumEvent::TransfersToNamada {
                    nonce: self.emit_next_bridge_nonce().into(),
                    transfers,
                }
            }
        }

        pub fn generate_transfer_to_namada(
            receiver: Address,
        ) -> TransferToNamada {
            let amount = arbitrary_amount();
            let asset = arbitrary_eth_address();
            TransferToNamada {
                amount,
                asset,
                receiver,
            }
        }
    }

    #[test]
    /// Test applying a `TransfersToNamada` batch containing a single transfer
    fn test_apply_updates_single_transfer() -> Result<()> {
        let sole_validator = address::testing::gen_established_address();

        let receiver = address::testing::gen_established_address();
        let sole_transfer =
            helpers::generate_transfer_to_namada(receiver.clone());
        let transfers = vec![sole_transfer.clone()];

        let mut bridge = helpers::FakeEthereumBridge::default();
        let event = bridge.emit_transfers_to_namada(transfers);

        let sole_validator_sighting =
            (sole_validator.clone(), BlockHeight(100));
        let update = EthMsgUpdate {
            body: event.clone(),
            seen_by: BTreeSet::from([sole_validator_sighting.clone()]),
        };
        let updates = HashSet::from([update]);
        let voting_powers = HashMap::from([(
            sole_validator_sighting,
            FractionalVotingPower::new(1, 1).unwrap(),
        )]);
        let mut storage = TestStorage::default();

        let changed_keys = apply_updates(&mut storage, updates, voting_powers)?;

        let eth_msg_keys: Keys<EthereumEvent> = (&event).into();
        let wrapped_erc20_keys: wrapped_erc20s::Keys =
            (&sole_transfer.asset).into();
        assert_eq!(
            BTreeSet::from([
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
                wrapped_erc20_keys.balance(&receiver),
                wrapped_erc20_keys.supply(),
            ]),
            changed_keys
        );

        let (body_bytes, _) = storage.read(&eth_msg_keys.body())?;
        let body_bytes = body_bytes.unwrap();
        assert_eq!(EthereumEvent::try_from_slice(&body_bytes)?, event);

        let (seen_bytes, _) = storage.read(&eth_msg_keys.seen())?;
        let seen_bytes = seen_bytes.unwrap();
        assert!(bool::try_from_slice(&seen_bytes)?);

        let (seen_by_bytes, _) = storage.read(&eth_msg_keys.seen_by())?;
        let seen_by_bytes = seen_by_bytes.unwrap();
        assert_eq!(
            Vec::<Address>::try_from_slice(&seen_by_bytes)?,
            vec![sole_validator]
        );

        let (voting_power_bytes, _) =
            storage.read(&eth_msg_keys.voting_power())?;
        let voting_power_bytes = voting_power_bytes.unwrap();
        assert_eq!(<(u64, u64)>::try_from_slice(&voting_power_bytes)?, (1, 1));

        let (wrapped_erc20_balance_bytes, _) =
            storage.read(&wrapped_erc20_keys.balance(&receiver))?;
        let wrapped_erc20_balance_bytes = wrapped_erc20_balance_bytes.unwrap();
        assert_eq!(
            Amount::try_from_slice(&wrapped_erc20_balance_bytes)?,
            sole_transfer.amount
        );

        let (wrapped_erc20_supply_bytes, _) =
            storage.read(&wrapped_erc20_keys.supply())?;
        let wrapped_erc20_supply_bytes = wrapped_erc20_supply_bytes.unwrap();
        assert_eq!(
            Amount::try_from_slice(&wrapped_erc20_supply_bytes)?,
            sole_transfer.amount
        );

        Ok(())
    }

    #[test]
    /// Test applying a single transfer via `apply_derived_tx`, where an event
    /// has enough voting power behind it for it to be applied at the same time
    /// that it is recorded in storage
    fn test_apply_derived_tx_new_event_mint_immediately() {
        let mut test =
            helpers::TestSetup::with_genesis_validator_voting_powers([
                100.into()
            ]);

        let receiver = address::testing::gen_established_address();
        let transfers =
            vec![helpers::generate_transfer_to_namada(receiver.clone())];

        let mut bridge = helpers::FakeEthereumBridge::default();
        let event = bridge.emit_transfers_to_namada(transfers);

        let tx_result = apply_derived_tx(
            &mut test.storage,
            vec![MultiSignedEthEvent {
                event: event.clone(),
                signers: BTreeSet::from([(
                    test.genesis_validators[0].clone(),
                    BlockHeight(100),
                )]),
            }],
        )
        .unwrap_or_else(|err| panic!("Test failed: {:#?}", err));

        assert_eq!(
            tx_result.gas_used, 0,
            "No gas should be used for a derived transaction"
        );
        let eth_msg_keys = vote_tracked::Keys::from(&event);
        let dai_keys = wrapped_erc20s::Keys::from(&DAI_ERC20_ETH_ADDRESS);
        assert_eq!(
            tx_result.changed_keys,
            BTreeSet::from([
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
                dai_keys.balance(&receiver),
                dai_keys.supply(),
            ])
        );
        assert!(tx_result.vps_result.accepted_vps.is_empty());
        assert!(tx_result.vps_result.rejected_vps.is_empty());
        assert!(tx_result.vps_result.errors.is_empty());
        assert!(tx_result.initialized_accounts.is_empty());
        assert!(tx_result.ibc_event.is_none());
    }

    /// Test calling apply_derived_tx for an event that isn't backed by enough
    /// voting power to be acted on immediately
    #[test]
    fn test_apply_derived_tx_new_event_dont_mint() {
        let equal_voting_power = 100;
        let mut test =
            helpers::TestSetup::with_genesis_validator_voting_powers([
                equal_voting_power.into(),
                equal_voting_power.into(),
            ]);

        let receiver = address::testing::gen_established_address();
        let transfers =
            vec![helpers::generate_transfer_to_namada(receiver.clone())];

        let mut bridge = helpers::FakeEthereumBridge::default();
        let event = bridge.emit_transfers_to_namada(transfers);

        let tx_result = apply_derived_tx(
            &mut test.storage,
            vec![MultiSignedEthEvent {
                event: event.clone(),
                signers: BTreeSet::from([(
                    test.genesis_validators[0].clone(),
                    BlockHeight(100),
                )]),
            }],
        )
        .unwrap_or_else(|err| panic!("Test failed: {:#?}", err));

        let eth_msg_keys = vote_tracked::Keys::from(&event);
        assert_eq!(
            tx_result.changed_keys,
            BTreeSet::from([
                eth_msg_keys.body(),
                eth_msg_keys.seen(),
                eth_msg_keys.seen_by(),
                eth_msg_keys.voting_power(),
            ]),
            "The Ethereum event should have been recorded, but no minting \
             should have happened yet as it has only been seen by 1/2 the \
             voting power so far"
        );
    }
}
