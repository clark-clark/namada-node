//! Proof-of-Stake system parameters

use borsh::{BorshDeserialize, BorshSerialize};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use thiserror::Error;

/// Proof-of-Stake system parameters
#[derive(Debug, Clone, BorshDeserialize, BorshSerialize)]
pub struct PosParams {
    /// A maximum number of active validators
    pub max_validator_slots: u64,
    /// Any change applied during an epoch `n` will become active at the
    /// beginning of epoch `n + pipeline_len`.
    pub pipeline_len: u64,
    /// How many epochs after a committed fault a validator can be slashed.
    /// If a fault is detected in epoch `n`, it can slashed up until the end of
    /// `n + slashable_period_len` epoch.
    /// The value must be greater or equal to `pipeline_len`.
    pub unbonding_len: u64,
    /// Used in validators' voting power calculation. Given in basis points
    /// (voting power per ten thousand tokens).
    pub votes_per_token: Decimal,
    /// Amount of tokens rewarded to a validator for proposing a block
    pub block_proposer_reward: Decimal,
    /// Amount of tokens rewarded to each validator that voted on a block
    /// proposal
    pub block_vote_reward: Decimal,
    /// Portion of validator's stake that should be slashed on a duplicate
    /// vote. Given in basis points (slashed amount per ten thousand tokens).
    pub duplicate_vote_slash_rate: Decimal,
    /// Portion of validator's stake that should be slashed on a light client
    /// attack. Given in basis points (slashed amount per ten thousand tokens).
    pub light_client_attack_slash_rate: Decimal,
}

impl Default for PosParams {
    fn default() -> Self {
        Self {
            max_validator_slots: 128,
            pipeline_len: 2,
            unbonding_len: 6,
            // 1 voting power per 1 fundamental token (10^6 per NAM or 1 per
            // namnam)
            votes_per_token: dec!(1.0),
            block_proposer_reward: dec!(0.0625),
            block_vote_reward: dec!(0.05),
            // slash 5%
            duplicate_vote_slash_rate: dec!(0.05),
            // slash 5%
            light_client_attack_slash_rate: dec!(0.05),
        }
    }
}

#[allow(missing_docs)]
#[derive(Error, Debug)]
pub enum ValidationError {
    #[error(
        "Maximum total voting power is too large: got {0}, expected at most \
         {MAX_TOTAL_VOTING_POWER}"
    )]
    TotalVotingPowerTooLarge(u64),
    #[error("Votes per token cannot be greater than 1, got {0}")]
    VotesPerTokenGreaterThanOne(Decimal),
    #[error("Pipeline length must be >= 2, got {0}")]
    PipelineLenTooShort(u64),
    #[error(
        "Unbonding length must be > pipeline length. Got unbonding: {0}, \
         pipeline: {1}"
    )]
    UnbondingLenTooShort(u64, u64),
}

/// The number of fundamental units per whole token of the native staking token
pub const TOKENS_PER_NAM: u64 = 1_000_000;

/// From Tendermint: <https://github.com/tendermint/tendermint/blob/master/spec/abci/apps.md#updating-the-validator-set>
const MAX_TOTAL_VOTING_POWER: i64 = i64::MAX / 8;

/// Assuming token amount is `u64` in micro units.
const TOKEN_MAX_AMOUNT: u64 = u64::MAX / TOKENS_PER_NAM;

impl PosParams {
    /// Validate PoS parameters values. Returns an empty list if the values are
    /// valid.
    #[must_use]
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = vec![];

        if self.pipeline_len < 2 {
            errors
                .push(ValidationError::PipelineLenTooShort(self.pipeline_len));
        }

        if self.pipeline_len >= self.unbonding_len {
            errors.push(ValidationError::UnbondingLenTooShort(
                self.unbonding_len,
                self.pipeline_len,
            ))
        }

        // Check maximum total voting power cannot get larger than what
        // Tendermint allows
        let max_total_voting_power = Decimal::from(self.max_validator_slots)
            * self.votes_per_token * Decimal::from(TOKEN_MAX_AMOUNT);
        match i64::try_from(max_total_voting_power) {
            Ok(max_total_voting_power_i64) => {
                if max_total_voting_power_i64 > MAX_TOTAL_VOTING_POWER {
                    errors.push(ValidationError::TotalVotingPowerTooLarge(
                        max_total_voting_power.to_u64().unwrap(),
                    ))
                }
            }
            Err(_) => errors.push(ValidationError::TotalVotingPowerTooLarge(
                max_total_voting_power.to_u64().unwrap(),
            )),
        }

        // Check that there is no more than 1 vote per token
        if self.votes_per_token > dec!(1.0) {
            errors.push(ValidationError::VotesPerTokenGreaterThanOne(
                self.votes_per_token,
            ))
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use testing::arb_pos_params;

    use super::*;

    proptest! {
        #[test]
        fn test_validate_arb_pos_params(pos_params in arb_pos_params()) {
            let errors = pos_params.validate();
            assert!(
                errors.is_empty(),
                "Arbitrary PoS parameters must be valid, `validate()` failed. \
                Parameters {:#?}\nErrors: {:#?}",
                pos_params,
                errors
            );
        }
    }
}

/// Testing helpers
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use proptest::prelude::*;

    use super::*;

    prop_compose! {
        /// Generate arbitrary valid ([`PosParams::validate`]) PoS parameters.
        pub fn arb_pos_params()
            (pipeline_len in 2..8_u64)
            (max_validator_slots in 1..128_u64,
            // `unbonding_len` > `pipeline_len`
            unbonding_len in pipeline_len + 1..pipeline_len + 8,
            pipeline_len in Just(pipeline_len),
            votes_per_token in 1..10_001_u64)
            -> PosParams {
            PosParams {
                max_validator_slots,
                pipeline_len,
                unbonding_len,
                votes_per_token: Decimal::from(votes_per_token) / dec!(10_000),
                // The rest of the parameters that are not being used in the PoS
                // VP are constant for now
                ..Default::default()
            }
        }
    }
}
