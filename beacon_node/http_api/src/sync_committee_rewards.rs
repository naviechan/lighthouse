use std::sync::Arc;
use beacon_chain::{BeaconChain, BeaconChainTypes};
use eth2::types::ValidatorId;
use eth2::lighthouse::{SyncCommitteeAttestationRewards, SyncCommitteeAttestationReward};
use slog::{debug, warn, Logger};
use state_processing::{per_block_processing::altair::sync_committee::compute_sync_aggregate_rewards};
use warp_utils::reject::{beacon_chain_error, custom_bad_request};
use crate::BlockId;

pub fn compute_sync_committee_rewards<T: BeaconChainTypes>(
    chain: Arc<BeaconChain<T>>,
    block_id: BlockId,
    validators: Vec<ValidatorId>,
    log: Logger
) -> Result<SyncCommitteeAttestationRewards, warp::Rejection> {

    let spec = &chain.spec;

    let (block, execution_optimistic) = block_id.blinded_block(&chain)?;

    let slot = block.slot();

    let state_root = block.state_root();

    let sync_aggregate = block
        .message()
        .body()
        .sync_aggregate()
        .map_err(|_| custom_bad_request(String::from("Unable to get sync aggregate")))?;

    // Technically we should use the pre-block state, but it won't matter because
    // compute_sync_aggregate_rewards() only uses state.get_total_active_balance() which only changes on epoch boundaries.
    // So, the "wrong" state will still give the same result.
    let mut state = chain
        .get_state(&state_root, Some(slot))
        .map_err(beacon_chain_error)?
        .ok_or_else(|| custom_bad_request(String::from("Unable to get state")))?;

    debug!(
        log,
        "Retrieving sync committee attestation rewards";
        "state_root" => ?state_root,
        "slot" => slot,
        );

    let (participant_reward_value, _) = compute_sync_aggregate_rewards(&state, spec)
        .map_err(|_| custom_bad_request(format!("Unable to get sync aggregate rewards at state root {:?}", state_root)))?;


    let current_sync_committee = state
        .current_sync_committee()
        .map_err(|_| custom_bad_request(String::from("Unable to get participants")))?
        .pubkeys
        .clone();

    debug!(
        log,
        "Retrived sync committee attestation reward value";
        "reward_value" => participant_reward_value,
        "sync_committee_participant_size" => current_sync_committee.len()
        );
    
    let data = if current_sync_committee.is_empty() { 
        None 
        } else {
            Some(
                current_sync_committee
                .iter()
                .zip(sync_aggregate.sync_committee_bits.iter())
                .map(|(sync_committee_pubkey, participation_bit)| {
                    let sync_committee_validator_index = match state.get_validator_index(sync_committee_pubkey) {
                                    Ok(validator_index) => validator_index,
                                    _ => Some(0)
                                }.unwrap();
                    (sync_committee_pubkey, sync_committee_validator_index, participation_bit)
                })
                .filter(|(sync_committee_pubkey, sync_committee_validator_index, participation_bit)| {
                    validators.is_empty()
                        ||
                    validators
                        .iter()
                        .any(|validator| match validator {
                            ValidatorId::PublicKey(pubkey) => {
                                *sync_committee_pubkey == pubkey
                            }
                            ValidatorId::Index(i) => {
                                *sync_committee_validator_index as u64 == *i
                            }
                        })
                })
                .map(|(_sync_committee_pubkey, sync_committee_validator_index, participation_bit)| {
                    SyncCommitteeAttestationReward {
                        validator_index: sync_committee_validator_index as u64,
                        reward: if participation_bit { participant_reward_value as i64 } 
                                else { participant_reward_value as i64 * -1 }
                    }
                })
                .collect::<Vec<_>>()
            )
        };

    Ok(SyncCommitteeAttestationRewards{
        execution_optimistic: None,
        finalized: None,
        data
    })
    
}
