#[cfg(feature = "ft2sft-impl")]
mod impl_;

use std::borrow::Cow;

use near_sdk::{
    ext_contract, near, serde_with::DisplayFromStr, AccountId, AccountIdRef, ContractCode,
    ContractStorage, NearToken,
};

use crate::{
    contract_state::ContractState,
    fungible_token::receiver::FungibleTokenReceiver,
    sharded_fungible_token::{
        minter::{ShardedFungibleTokenBurner, ShardedFungibleTokenMinter},
        wallet::TransferNotification,
    },
};

/// Fungible Tokens to Sharded Fungible Tokens adaptor.
/// It mints sharded fungible tokens on [`.ft_on_transfer()`](crate::fungible_token::receiver::FungibleTokenReceiver::ft_on_transfer)
/// and burns them back in [`.sft_on_burn()`](crate::sharded_fungible_token::minter::ShardedFungibleTokenBurner::sft_on_burn).
#[ext_contract(ext_ft2ft)]
pub trait Ft2Sft:
    ShardedFungibleTokenMinter + ShardedFungibleTokenBurner + FungibleTokenReceiver
{
    fn ft2sft_minter_data(self) -> ContractState<Ft2SftData<'static>>;
    // TODO: admin utils? transfer from child, lock child, etc...
}

#[near(serializers = [borsh, json])]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ft2SftData<'a> {
    /// Total amount of fungible tokens minted
    #[serde_as(as = "DisplayFromStr")]
    pub total_supply: u128,

    /// Contract implementing NEP-141 fungible token standard
    pub ft_contract_id: Cow<'a, AccountIdRef>,

    /// Code for deploying child wallet-contracts
    pub sft_wallet_code: ContractCode,
}

/// Message for [`.ft_on_transfer()`](crate::fungible_token::receiver::FungibleTokenReceiver::ft_on_transfer)
#[near(serializers = [borsh, json])]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintMessage {
    /// Receiver of the sharded FTs, or `sender_id` if not given
    pub receiver_id: Option<AccountId>,

    /// Memo to pass in [`.sft_receive()`](crate::sharded_fungible_token::wallet::ShardedFungibleTokenWallet::sft_receive)
    pub memo: Option<String>,

    /// Optionally, notify `receiver_id` via [`.sft_on_receive()`](crate::sharded_fungible_token::receiver::ShardedFungibleTokenReceiver::sft_on_receive).
    /// Note that non-zero [`forward_deposit`](TransferNotification::forward_deposit)
    /// and [`state_init.state_init_amount`](crate::sharded_fungible_token::wallet::StateInitArgs::state_init_amount)
    /// are not supported, since [`.ft_on_transfer()`](crate::fungible_token::receiver::FungibleTokenReceiver::ft_on_transfer)
    /// doesn't support attaching deposit according to NEP-141 spec.
    pub notify: Option<TransferNotification>,
}

/// TODO: docs
#[near(serializers = [borsh, json])]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BurnMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_id: Option<AccountId>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,

    #[serde(default, skip_serializing_if = "NearToken::is_zero")]
    pub storage_deposit: NearToken,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund_to: Option<AccountId>,
}

impl<'a> Ft2SftData<'a> {
    const STATE_KEY: &'static [u8] = b"";

    #[inline]
    pub fn init(
        ft_contract_id: impl Into<Cow<'a, AccountIdRef>>,
        sft_wallet_code: impl Into<ContractCode>,
    ) -> Self {
        Self {
            total_supply: 0,
            ft_contract_id: ft_contract_id.into(),
            sft_wallet_code: sft_wallet_code.into(),
        }
    }

    #[inline]
    pub fn init_state(
        ft_contract_id: impl Into<Cow<'a, AccountIdRef>>,
        sft_wallet_code: impl Into<ContractCode>,
    ) -> ContractStorage {
        ContractStorage::new().borsh(&Self::STATE_KEY, &Self::init(ft_contract_id, sft_wallet_code))
    }
}
