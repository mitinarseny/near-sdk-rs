use near_sdk::{ext_contract, json_types::U128, AccountId, PromiseOrValue};

/// Receiver (i.e. owner_id) of sharded fungible tokens
#[ext_contract(ext_sft_receiver)]
pub trait ShardedFungibleTokenReceiver {
    /// Called by wallet-contract upon receiving tokens.
    ///
    /// TODO DO NOT BLINDLY TRUST `sender_id`, verify the minter first
    ///
    /// TODO NOTE: amount can be zero
    ///
    /// Returns number of used tokens, indicating `amount - used` should be
    /// refunded back to the `sender_id`.
    ///
    /// There are two possible ways to get `minter_id` of just received
    /// tokens:
    /// * Pass it in `msg`, so it can be verified using
    ///   [`::near_sdk::StateInit::derived_account_id()`]
    /// * Call view-method `predecessor_account_id::sft_wallet_data()` and
    ///   extract `data.minter_id`
    /// Note: implementations are recommended to be `#[payable]`.
    fn sft_on_receive(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
        // TODO: pass jetton-wallet code?
    ) -> PromiseOrValue<U128>;
}
