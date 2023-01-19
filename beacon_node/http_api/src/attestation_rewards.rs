use std::{sync::Arc, collections::HashMap};
use beacon_chain::{BeaconChain, BeaconChainTypes};
use eth2::{lighthouse::AttestationRewardsTBD, types::ValidatorId};
use eth2::lighthouse::attestation_rewards::{IdealAttestationRewards, TotalAttestationRewards};
use safe_arith::SafeArith;
use slog::Logger;
use participation_cache::ParticipationCache;
use state_processing::{per_epoch_processing::altair::{participation_cache, rewards_and_penalties::get_flag_weight}, common::altair::{BaseRewardPerIncrement, get_base_reward}};
use types::{Epoch, EthSpec};
use types::consts::altair::WEIGHT_DENOMINATOR;
use types::consts::altair::{TIMELY_SOURCE_FLAG_INDEX, TIMELY_TARGET_FLAG_INDEX, TIMELY_HEAD_FLAG_INDEX};
use warp_utils::reject::{custom_not_found};

pub fn compute_attestation_rewards<T: BeaconChainTypes>(
    chain: Arc<BeaconChain<T>>,
    epoch: Epoch,
    validators: Vec<ValidatorId>,
    log: Logger
) -> Result<AttestationRewardsTBD, warp::Rejection> {    

    //--- Get state ---//
    let spec = &chain.spec;

    let state_slot = (epoch + 1).end_slot(T::EthSpec::slots_per_epoch());

    let state_root = chain
        .state_root_at_slot(state_slot)
        .map_err(warp_utils::reject::beacon_chain_error)?
        .ok_or(warp_utils::reject::custom_not_found("State root not found".to_owned()))?;

    let state = chain
        .get_state(&state_root, Some(state_slot))
        .map_err(warp_utils::reject::beacon_chain_error)?
        .ok_or(warp_utils::reject::custom_not_found("State not found".to_owned()))?;
    
    //--- Calculate ideal_rewards ---//
    let participation_cache = ParticipationCache::new(&state, spec)
        .map_err(|e| custom_not_found(format!("Unable to get participation_cache! {:?}", e)))?;
    
    let previous_epoch = state.previous_epoch();

    let mut ideal_rewards_hashmap = HashMap::new();

    let flag_index = 0;
    let weight = 0;
    let base_reward = 0;
    let effective_balance_eth = 0;

    for flag_index in [TIMELY_SOURCE_FLAG_INDEX, TIMELY_TARGET_FLAG_INDEX, TIMELY_HEAD_FLAG_INDEX].iter() {

        let weight = get_flag_weight(*flag_index)
            .map_err(|e| custom_not_found(format!("Unable to get weight! {:?}", e)))?;

        let unslashed_participating_indices = participation_cache.get_unslashed_participating_indices(*flag_index, previous_epoch)
            .map_err(|e| custom_not_found(format!("Unable to get unslashed_participating_indices! {:?}", e)))?;   

        let unslashed_participating_balance = unslashed_participating_indices.total_balance()
            .map_err(|e| custom_not_found(format!("Unable to get unslashed_participating_balance! {:?}", e)))?;    
        
        let unslashed_participating_increments = unslashed_participating_balance.safe_div(spec.effective_balance_increment)
            .map_err(|e| custom_not_found(format!("Unable to get unslashed_participating_increments! {:?}", e)))?;

        let total_active_balance = participation_cache.current_epoch_total_active_balance();
        
        let active_increments = total_active_balance.safe_div(spec.effective_balance_increment)
            .map_err(|e| custom_not_found(format!("Unable to get active_increments! {:?}", e)))?;            
        
        let base_reward_per_increment = BaseRewardPerIncrement::new(total_active_balance, spec)
            .map_err(|e| custom_not_found(format!("Unable to get base_reward_per_increment! {:?}", e)))?;
        
        for effective_balance_eth in 0..=32 {
            
            let base_reward = get_base_reward(&state, effective_balance_eth, base_reward_per_increment, spec);

            let base_reward = base_reward
                .map_err(|e| warp_utils::reject::custom_not_found(format!("Unable to get base_reward! {:?}", e)))?;

            let reward_numerator = base_reward
                .safe_mul(weight)
                .and_then(|reward_numerator| reward_numerator.safe_mul(unslashed_participating_increments))
                .map_err(|_| warp_utils::reject::custom_server_error("Unable to calculate reward numerator".to_owned()))?;
        
            let reward = reward_numerator
                .safe_div(active_increments)
                .and_then(|reward| reward.safe_div(WEIGHT_DENOMINATOR))
                .map_err(|_| warp_utils::reject::custom_server_error("Unable to calculate reward".to_owned()))?;
            
            if !state.is_in_inactivity_leak(previous_epoch, &spec) {
                ideal_rewards_hashmap.insert((flag_index, effective_balance_eth), reward);
            } else {
                ideal_rewards_hashmap.insert((flag_index, effective_balance_eth), 0);
            }  
        }
    }

    //--- Calculate actual rewards ---//
    let mut rewards = Vec::new();

    let index = participation_cache.eligible_validator_indices();

    for validator_index in index {

        let eligible = state.is_eligible_validator(previous_epoch, *validator_index)
        .map_err(|_| warp_utils::reject::custom_server_error("Unable to get eligible".to_owned()))?;

        let total_reward = if !eligible {
            0u64
        } else {
            let voted_correctly = participation_cache.get_unslashed_participating_indices(flag_index, previous_epoch).is_ok();
            if voted_correctly {
                *ideal_rewards_hashmap.get(&(&flag_index, effective_balance_eth)).unwrap_or(&0)
            } else {
                (-(base_reward as i64 as i128) * weight as i128 / WEIGHT_DENOMINATOR as i128) as u64
            }
        };
        rewards.push((*validator_index, total_reward));
    }

    //TODO Check target and source
    let ideal_rewards: Vec<IdealAttestationRewards> = ideal_rewards_hashmap.iter().map(|((flag_index, effective_balance_eth), reward)| {
        IdealAttestationRewards {
            effective_balance: *effective_balance_eth as u64,
            head: *reward,
            target: 0,
            source: 0,
        }
    }).collect();

    //TODO Check target, source, and inclusion_delay
    let total_rewards: Vec<TotalAttestationRewards> = rewards.into_iter().map(|(validator_index, reward)| {
        TotalAttestationRewards {
            validator_index: validator_index as u64,
            head: reward as i64,
            target: 0,
            source: 0,
            inclusion_delay: 0,
        }
    }).collect();

    Ok(AttestationRewardsTBD{
        execution_optimistic: false,
        finalized: false,
        ideal_rewards,
        total_rewards,
    })
}