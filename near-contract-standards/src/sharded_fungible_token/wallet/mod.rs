#[cfg(feature = "sft-wallet-impl")]
mod impl_;

use std::borrow::Cow;

use near_sdk::{
    ext_contract,
    json_types::U128,
    near,
    serde_with::{serde_as, DisplayFromStr},
    AccountId, AccountIdRef, ContractStorage, Gas, LazyStateInit, NearToken, PromiseOrValue,
};

use crate::contract_state::ContractState;

/// # Sharded Fungible Token wallet-contract
//
/// The design is highly inspired by [Jetton](https://docs.ton.org/v3/guidelines/dapps/asset-processing/jettons#jetton-architecture)
/// standard except for following differences:
/// * Unlike TVM, Near doesn't support [message bouncing](https://docs.ton.org/v3/documentation/smart-contracts/transaction-fees/forward-fees#message-bouncing),
///   so instead we can schedule callbacks, which gives more control over
///   handling of failed cross-contract calls.
/// * TVM doesn't differentiate between gas and attached deposit, while
///   in Near they are not coupled, which removes some complexities.
///
/// ## Events
///
/// Similar to Jetton standard, there is no logging of such events as
/// `sft_transfer`, `sft_mint` or `sft_burn` as it simply wouldn't bring any
/// value for indexers. Even if we do emit these events, indexers are still
/// forced to track `sft_transfer` function calls to not-yet-existing
/// wallet-contracts, which will emit these events.
/// However, to properly track these cross-contract calls they would need
/// parse function names (i.e. `sft_transfer()`, `sft_receive()`, `sft_burn()`
/// and `sft_resolve()`) and their args, while this information combined with
/// receipt status already contains all necessary info for indexing.
#[ext_contract(ext_sft_wallet)]
pub trait ShardedFungibleTokenWallet {
    /// View method to get all data at once
    fn sft_wallet_data(self) -> ContractState<SFTWalletData<'static>>;

    /// Transfer given `amount` of tokens to `receiver_id`.
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
    ///
    /// Note: must be #[payable]
    fn sft_transfer(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>, // TODO: custom_payload
        notify: Option<TransferNotification>,
        // TODO: rename: refund_deposit_to?
        refund_to: Option<AccountId>,
        no_init: Option<bool>,
        // TODO: custom_payload (e.g. mintless jetton)
        // https://github.com/ton-blockchain/mintless-jetton-contract/blob/77763e6b2df6cf96b0840c7306844751200ff046/contracts/jetton-wallet.fc#L59
        // TODO: delete_my_if_zero
    ) -> PromiseOrValue<U128>;

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
    ///
    /// Note: must be #[payable] and require at least 1yN attached.
    fn sft_receive(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        memo: Option<String>,
        notify: Option<TransferNotification>,
        refund_to: Option<AccountId>,
    ) -> PromiseOrValue<U128>;

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
    /// Returns `burned_amount`.
    ///
    /// Note: must be #[payable] and require at least 1yN attached
    fn sft_burn(&mut self, amount: U128, msg: String) -> PromiseOrValue<U128>;
}

/// Sharded Fungible Token wallet-contract data
#[near(serializers = [borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SFTWalletData<'a> {
    pub status: u8, // TODO: #[cfg(feature = "sft-wallet-governed")]?
    #[serde_as(as = "DisplayFromStr")]
    pub balance: u128,
    pub owner_id: Cow<'a, AccountIdRef>,
    pub minter_id: Cow<'a, AccountIdRef>,
    // TODO: extra T
}

impl<'a> SFTWalletData<'a> {
    const STATE_KEY: &'static [u8] = b"";
    // TODO: calculate exact values
    pub const SFT_RECEIVE_MIN_GAS: Gas = Gas::from_tgas(5);
    pub const SFT_RESOLVE_GAS: Gas = Gas::from_tgas(5);

    #[inline]
    pub fn init(
        owner_id: impl Into<Cow<'a, AccountIdRef>>,
        minter_id: impl Into<Cow<'a, AccountIdRef>>,
    ) -> Self {
        Self { status: 0, balance: 0, owner_id: owner_id.into(), minter_id: minter_id.into() }
    }

    #[inline]
    pub fn init_state(
        owner_id: impl Into<Cow<'a, AccountIdRef>>,
        minter_id: impl Into<Cow<'a, AccountIdRef>>,
    ) -> ContractStorage {
        ContractStorage::new().borsh(&Self::STATE_KEY, &Self::init(owner_id, minter_id))
    }
}

/// Arguments for constructing [`receiver_id::sft_on_receive()`](super::receiver::ShardedFungibleTokenReceiver::sft_on_receive) notification.
#[near(serializers = [borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferNotification {
    /// Optionally, deploy & init `receiver_id` contract if didn't exist.
    /// It enables for better composability when transferring to other
    /// not-yet-initialized owner contracts.
    #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
    pub state_init: Option<StateInitArgs>,

    /// Message to pass in [`receiver_id::sft_on_transfer()`](super::receiver::ShardedFungibleTokenReceiver::sft_on_transfer)
    pub msg: String,

    /// Amount of NEAR tokens to attach to `receiver_id::sft_on_transfer()` call.
    #[serde(default, skip_serializing_if = "NearToken::is_zero")]
    pub forward_deposit: NearToken,
}

impl TransferNotification {
    #[inline]
    pub fn msg(msg: String) -> Self {
        Self { state_init: None, msg, forward_deposit: NearToken::from_yoctonear(0) }
    }

    #[inline]
    pub fn state_init(mut self, state_init: LazyStateInit, amount: NearToken) -> Self {
        self.state_init = Some(StateInitArgs { state_init, state_init_amount: amount });
        self
    }

    #[inline]
    pub fn forward_deposit(mut self, amount: NearToken) -> Self {
        self.forward_deposit = amount;
        self
    }
}

/// TODO: docs
#[near(serializers=[borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInitArgs {
    pub state_init: LazyStateInit,

    #[serde(default, skip_serializing_if = "NearToken::is_zero")]
    pub state_init_amount: NearToken,
}
