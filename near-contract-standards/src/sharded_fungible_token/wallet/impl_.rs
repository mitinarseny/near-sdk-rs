use impl_tools::autoimpl;
use near_sdk::{
    env, json_types::U128, near, require, serde_json, AccountId, AccountIdRef, NearToken,
    PanicOnDefault, Promise, PromiseOrValue, StateInit,
};

use crate::{
    contract_state::ContractState,
    sharded_fungible_token::{
        minter::ext_sft_burner,
        receiver::ext_sft_receiver,
        wallet::{SFTWalletData, ShardedFungibleTokenWallet, StateInitArgs, TransferNotification},
    },
};

/// Reference implementation of Sharded Fungible Foken wallet-contract.
///
/// This implementation should be globally deployed only once and can
/// then be reused for different minter-contracts. Owners might reference
/// its globally deployed code to calculate [`StateInit`] and verify
/// authenticity of [`env::predecessor_account_id()`] via
/// [`.derived_account_id()`](StateInit::derived_account_id).
///
/// The implementation is highly inspired by [Jetton wallet](https://github.com/ton-blockchain/jetton-contract/blob/3d24b419f2ce49c09abf6b8703998187fe358ec9/contracts/jetton-wallet.fc)
/// contract reference implementation.  
/// See [wallet-contract documentation](ShardedFungibleTokenWallet).
#[near(contract_state(key = SFTWalletData::STATE_KEY))]
#[autoimpl(Deref using self.0)]
#[autoimpl(DerefMut using self.0)]
#[derive(PanicOnDefault)]
#[repr(transparent)]
struct SFTWalletContract(SFTWalletData<'static>);

#[near]
impl ShardedFungibleTokenWallet for SFTWalletContract {
    /// View method to get all data at once
    fn sft_wallet_data(self) -> ContractState<SFTWalletData<'static>> {
        ContractState { code: env::current_contract_code(), state: self.0 }
    }

    /// Transfer given `amount` of fungible tokens to `receiver_id`.
    ///
    /// Requires at least 1 yoctoNear attached deposit.
    ///
    /// If `init_receiver_wallet_or_refund_to` is set, then requires
    /// at least [`ShardedFungibleTokenWalletData::MIN_BALANCE`]
    /// attached deposit to reserve for deploying receiver's
    /// wallet-contract if it doesn't exist. If it turned out to be
    /// already deployed, then reserved NEAR tokens are sent to
    /// `init_receiver_wallet_or_refund_to`.
    ///
    /// If `notify` is set, then `receiver_id::sft_on_transfer()`
    /// will be called. If `notify.state_init` is set, then
    /// `receiver_id` will be initialized if doesn't exist.
    ///
    /// Remaining attached deposit is forwarded to `receiver_id::sft_on_transfer()`.
    ///
    /// Returns `used_amount`.
    #[payable]
    fn sft_transfer<'nearinput>(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        notify: Option<TransferNotification>,
        refund_to: Option<AccountId>,
        no_init: Option<bool>,
    ) -> PromiseOrValue<U128> {
        let caller = env::predecessor_account_id();
        require!(
            // TODO: governance: status?
            caller == *self.owner_id,
            ERR_NOT_OWNER,
        );
        require!(receiver_id != *self.owner_id, ERR_SELF_TRANSFER);

        // We do not require `amount > 0`, since it can be used to just pay for
        // receiver's wallet-contract creation. Optionally, the receiver
        // contract can be notified about it, so he can rely on its existense.

        self.balance = self
            .balance
            .checked_sub(amount.0)
            .unwrap_or_else(|| env::panic_str(ERR_INSUFFICIENT_BALANCE));

        let (state_init_amount, state_init) = {
            let state_init = self.sft_wallet_init_for(&receiver_id);
            (
                no_init.is_none_or(|b| !b).then(|| state_init.storage_cost()),
                state_init.lazy_serialized(), // do not serialize twice
            )
        };

        let mut deposit_left = env::attached_deposit();

        // refund to given account or caller if not specified
        let refund_to = refund_to.unwrap_or(caller);

        // call receiver's wallet-contract
        let mut p = Promise::new(state_init.derive_account_id())
            // refund state_init amount and attached deposit in case of failure
            // to `refund_to` instead of sender's wallet-contract
            .refund_to(refund_to.clone());

        if let Some(amount) = state_init_amount {
            // subtract the required amount for state_init from attached deposit
            deposit_left =
                deposit_left.checked_sub(amount).unwrap_or_else(|| env::panic_str(ERR_DEPOSIT));

            // deploy & init receiver's wallet-contract
            p = p.state_init(state_init, amount);
        }

        // we still need at least 1yN to attach to `.sft_receive()`
        require!(deposit_left >= NearToken::from_yoctonear(1), ERR_DEPOSIT);

        Self::ext_on(p)
            // forward remaining attached deposit
            .with_attached_deposit(deposit_left)
            // require minimum gas
            .with_static_gas(SFTWalletData::SFT_RECEIVE_MIN_GAS)
            // forward all remaining gas here
            .with_unused_gas_weight(1)
            .sft_receive(
                self.owner_id.clone().into_owned(),
                amount,
                memo,
                notify,
                Some(refund_to.into()),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SFTWalletData::SFT_RESOLVE_GAS)
                    // do not distribute remaining gas here
                    .with_unused_gas_weight(0)
                    .sft_resolve(amount, true),
            )
            .into()
    }

    /// Receives tokens from minter-contract or wallet-contracts initialized
    /// for the same minter-contract.
    ///
    /// If `notify` is set, then `receiver_id::sft_on_transfer()` will be
    /// called. If `notify.state_init` is set, then `receiver_id` will be
    /// initialized if doesn't exist.
    ///
    /// Remaining attached deposit is forwarded to `receiver_id::sft_on_transfer()`.
    ///
    /// Returns `used_amount`.
    /// TODO: maybe return unused_amount from everywhere, so it's:
    /// 1. cheaper on full transfers
    /// 2. easier for other contracts to keep track of refunds by trusting
    ///   sFT wallet contracts
    #[payable]
    fn sft_receive<'nearinput>(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        // memo will be stored in receipt's FunctionCall args anyway
        #[allow(unused_variables)] memo: Option<String>,
        notify: Option<TransferNotification>,
        refund_to: Option<AccountId>,
    ) -> PromiseOrValue<U128> {
        let mut deposit_left = env::attached_deposit();
        require!(deposit_left >= NearToken::from_yoctonear(1), ERR_DEPOSIT);

        let caller = env::predecessor_account_id();
        // verify that the caller is a valid wallet-contract or the minter
        require!(
            caller == *self.minter_id || caller == self.sft_wallet_account_id_for(&caller),
            ERR_WRONG_WALLET,
        );

        self.balance = self
            .balance
            .checked_add(amount.0)
            .unwrap_or_else(|| env::panic_str(ERR_BALANCE_OVERFLOW));

        let Some(notify) = notify else {
            // no transfer notification, all tokens received
            return PromiseOrValue::Value(amount);
        };

        // refund to given account or `sender_id` if not specified
        let refund_to = refund_to.unwrap_or_else(|| sender_id.clone());

        // notify receiver
        let mut p = Promise::new(self.owner_id.clone().into_owned())
            // refund state_init amount and attached deposit in case of failure
            // to `refund_to` instead of receiver's wallet-contract
            .refund_to(refund_to.clone());

        if let Some(StateInitArgs { state_init, state_init_amount }) = notify.state_init {
            deposit_left = deposit_left
                .checked_sub(state_init_amount)
                .unwrap_or_else(|| env::panic_str(ERR_DEPOSIT));

            p = p.state_init(state_init, state_init_amount);
        }

        // check that there was enough attached deposit
        deposit_left = deposit_left
            .checked_sub(notify.forward_deposit)
            .unwrap_or_else(|| env::panic_str(ERR_DEPOSIT));

        // refund excess deposit (only if more than 1yN)
        if deposit_left > NearToken::from_yoctonear(1) {
            // detached
            let _ = Promise::new(refund_to.clone()).transfer(deposit_left);
        }

        ext_sft_receiver::ext_on(p)
            // forward deposit
            .with_attached_deposit(notify.forward_deposit)
            // forward all remaining gas here
            .with_unused_gas_weight(1)
            .sft_on_receive(sender_id, amount, notify.msg)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SFTWalletData::SFT_RESOLVE_GAS)
                    // do not distribute remaining gas here
                    .with_unused_gas_weight(0)
                    // resolve notification
                    .sft_resolve(amount, false),
            )
            .into()
    }

    /// Burn given `amount` and notify [`minter_id::sft_on_burn()`](super::minter::SharedFungibleTokenBurner::sft_on_burn).
    /// If `minter_id` doesn't support burning or returns partial
    /// `used_amount`, then `amount - used_amount` will be minter back
    /// on `sender_id`.
    ///
    /// Code of this wallet-contract will be re-used across all applications
    /// that want to interact with sharded fungible tokens, so we need a
    /// uniform method to burn tokens to be supported by every wallet-contract.
    /// If the minter-contract doesn't support burning, these tokens
    /// will be minted back on burner wallet-contract.
    ///
    /// Returns `burned_amount`
    #[payable]
    fn sft_burn<'nearinput>(&mut self, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let deposit = env::attached_deposit();
        require!(deposit >= NearToken::from_yoctonear(1), ERR_DEPOSIT);
        require!(env::predecessor_account_id() == *self.owner_id, ERR_NOT_OWNER);

        self.balance = self
            .balance
            .checked_sub(amount.0)
            .unwrap_or_else(|| env::panic_str(ERR_INSUFFICIENT_BALANCE));

        ext_sft_burner::ext(self.minter_id.as_ref().to_owned())
            // forward all attached deposit
            .with_attached_deposit(deposit)
            // forward all remaining gas here
            .with_unused_gas_weight(1)
            .sft_on_burn(self.owner_id.clone().into_owned(), amount, msg)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SFTWalletData::SFT_RESOLVE_GAS)
                    // do not distribute remaining gas here
                    .with_unused_gas_weight(0)
                    .sft_resolve(amount, true),
            )
            .into()
    }
}

#[near]
impl SFTWalletContract {
    const MAX_RESULT_LENGTH: u64 = "\"340282366920938463463374607431768211455\"".len() as _; // u128::MAX

    /// Gets result from `sft_receive()`, `sft_on_transfer()`
    /// or `sft_on_burn()`, adjusts the balance accordingly
    /// and returns `used_amount`.
    #[allow(dead_code)]
    #[private]
    pub fn sft_resolve(&mut self, amount: U128, sender: bool) -> U128 {
        let mut used_amount = env::promise_result_at_most(
            0,
            Self::MAX_RESULT_LENGTH, // prevent out of gas (too long result)
        )
        .ok() // promise failed
        .and_then(Result::ok) // result is too long
        .and_then(|data| serde_json::from_slice::<U128>(&data).ok()) // JSON
        .unwrap_or_default() // if any of above failed, then refund full amount
        .0
        .min(amount.0); // do not refund more than we sent

        let mut refund_amount = amount.0.saturating_sub(used_amount);

        self.balance = if sender {
            // add refund_amount to sender, but in checked way:
            // faulty minter-contract implementation could have minted
            // too many tokens after `.sft_resolve()` was scheduled
            // but not executed yet
            self.balance
                .checked_add(refund_amount)
                // this is the only place where we do panic but it's ok,
                // since it can only happen because of the faulty minter
                .unwrap_or_else(|| env::panic_str(ERR_BALANCE_OVERFLOW))
        } else {
            // refund maximum what we can
            refund_amount = refund_amount.min(self.balance);
            // update used_amount
            used_amount = amount.0.saturating_sub(refund_amount);
            // subtract refund from receiver
            self.balance.saturating_sub(refund_amount)
        };

        U128(used_amount)
    }
}

impl SFTWalletContract {
    #[inline]
    pub fn sft_wallet_init_for(&self, owner_id: &AccountIdRef) -> StateInit {
        StateInit::code(env::current_contract_code())
            .data(SFTWalletData::init_state(owner_id, &*self.minter_id))
    }

    #[inline]
    pub fn sft_wallet_account_id_for(&self, owner_id: &AccountIdRef) -> AccountId {
        self.sft_wallet_init_for(owner_id).derive_account_id()
    }
}

const ERR_NOT_OWNER: &str = "not owner";
const ERR_SELF_TRANSFER: &str = "self-transfer";
const ERR_WRONG_WALLET: &str = "wrong wallet";
const ERR_INSUFFICIENT_BALANCE: &str = "insufficient balance";
const ERR_BALANCE_OVERFLOW: &str = "balance overflow";
const ERR_DEPOSIT: &str = "insufficient attached deposit";

#[cfg(test)]
mod tests {
    use std::u128;

    use super::*;

    #[test]
    fn promise_result_ok() {
        let result = serde_json::to_string_pretty(&U128(u128::MAX)).unwrap();
        assert!(result.len() as u64 <= SFTWalletContract::MAX_RESULT_LENGTH);
    }

    #[test]
    fn promise_result_too_long() {
        let result = serde_json::to_string_pretty(&"9".repeat(100)).unwrap();
        assert!(result.len() as u64 > SFTWalletContract::MAX_RESULT_LENGTH);
    }
}
