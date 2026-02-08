---
NEP: XXXX
Title: Sharded Fungible Token
Authors: Arseny Mitin <https://github.com/mitinarseny>
Status: Draft
DiscussionsTo: TBD
Type: Standards Track
Category: Contract
Version: 1.0.0
Created: 2026-02-08
LastUpdated: 2026-02-08
Requires: NEP-616
---

## Summary

This proposal introduces a **Sharded Fungible Token (SFT)** standard for NEAR Protocol, in which each token owner's balance is held in a separate, deterministically addressed wallet-contract rather than in a single centralized token contract. The minter-contract manages total supply and wallet code, while per-owner wallet-contracts hold individual balances and execute transfers directly between each other. Wallet-contracts are lazily deployed via the `StateInit` mechanism defined in [NEP-616] and fit within Zero Balance Account limits, requiring no storage staking. The design is inspired by the [Jetton standard](https://docs.ton.org/v3/guidelines/dapps/asset-processing/jettons) on TON, adapted for NEAR's asynchronous execution model with scheduled callbacks instead of message bouncing.

## Motivation

The current fungible token standard ([NEP-141]) stores all user balances in a single contract's state. This creates several problems at scale:

- **State bloat**: As the number of token holders grows, the contract's state grows linearly, increasing storage costs and serialization overhead.
- **Single-shard bottleneck**: All token operations are funneled through a single contract on a single shard, limiting throughput for popular tokens.
- **No parallelism**: Transfers between unrelated parties cannot execute concurrently because they compete for the same contract's state.

By distributing balances across per-owner wallet-contracts, the Sharded Fungible Token standard:

1. **Eliminates state bloat** in the minter-contract — each wallet stores only its own ~130 bytes of state.
2. **Enables cross-shard parallelism** — transfers between different owner pairs can execute on different shards simultaneously.
3. **Reduces storage costs** — wallet-contracts fit within NEAR's Zero Balance Account (ZBA) limit of 770 bytes, requiring no storage staking.
4. **Preserves composability** — wallet-contracts are regular NEAR accounts with deterministic addresses, allowing any contract to verify wallet authenticity offline via `StateInit::derive_account_id()`.
5. **Reuses wallet code globally** — a single globally deployed wallet contract binary serves all minter-contracts, amortizing deployment costs.

With [NEP-616] providing the foundation for deterministic account IDs and `StateInit`-based deployment, the ecosystem gains the building block needed for a scalable, sharded token standard.

## Specification

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [RFC 2119](https://www.ietf.org/rfc/rfc2119.txt).

### Architecture Overview

A Sharded Fungible Token deployment consists of:

1. **Minter-contract**: A single contract managing total supply and holding a reference to the globally deployed wallet code. Responsible for minting new tokens (crediting wallet-contracts) and optionally handling burn callbacks.
2. **Wallet-contracts**: Per-owner contracts deployed lazily via `StateInit`. Each stores the owner's balance, the owner's `AccountId`, and the minter's `AccountId`. Transfers happen directly between wallet-contracts.
3. **Receiver interface**: An optional interface for contracts that want to be notified of incoming token transfers and act on them programmatically.

```
┌─────────────────┐
│  Minter Contract│  (total_supply, sft_wallet_code)
│  (one per token) │
└────────┬────────┘
         │ mints to / burns from
         ▼
┌──────────────┐    sft_send/sft_receive    ┌──────────────┐
│ Wallet(Alice)│ ◄─────────────────────────► │ Wallet(Bob)  │
│  balance: 100│                             │  balance: 50 │
│  owner: alice│                             │  owner: bob  │
│  minter: M   │                             │  minter: M   │
└──────────────┘                             └──────────────┘
  deterministic                                deterministic
  AccountId via                                AccountId via
  StateInit                                    StateInit
```

### Data Structures

#### `SftMinterData`

Common data stored by all minter-contract implementations.

```json
{
  "sft_wallet_code": "<GlobalContractId>",
  "total_supply": "<u128 as string>"
}
```

| Field             | Type               | Description                                            |
|-------------------|--------------------|--------------------------------------------------------|
| `sft_wallet_code` | `GlobalContractId` | Reference to globally deployed wallet contract code    |
| `total_supply`    | `U128`             | Total amount of fungible tokens in circulation         |

`GlobalContractId` is defined in [NEP-616] and references contract code either by hash or by the account ID of a globally deployed contract.

#### `SftWalletData`

Data stored in each wallet-contract instance.

```json
{
  "minter_id": "<AccountId>",
  "owner_id": "<AccountId>",
  "balance": "<u128 as string>"
}
```

| Field       | Type        | Description                           |
|-------------|-------------|---------------------------------------|
| `minter_id` | `AccountId` | Account ID of the minter-contract     |
| `owner_id`  | `AccountId` | Account ID of the token owner         |
| `balance`   | `U128`      | Owner's token balance                 |

Wallet state MUST be stored under the empty key `b""` using Borsh serialization to minimize storage overhead.

#### `TransferNotification`

Arguments for constructing a notification to the receiver upon token transfer.

```json
{
  "msg": "<string>",
  "state_init": "<StateInit, optional>",
  "state_init_amount": "<NearToken, optional>"
}
```

| Field               | Type                 | Description                                                        |
|---------------------|----------------------|--------------------------------------------------------------------|
| `msg`               | `String`             | Application-specific message passed to `sft_on_receive()`          |
| `state_init`        | `StateInit` (optional) | If set, deploy & initialize the receiver contract if it doesn't exist |
| `state_init_amount` | `NearToken` (optional) | Amount of NEAR to attach for the receiver's `StateInit` deployment  |

### Minter-Contract Interface

#### `ShardedFungibleTokenMinter`

Every minter-contract MUST implement this interface.

```rust
pub trait ShardedFungibleTokenMinter {
    /// View method returning all minter data.
    fn sft_minter_data(self) -> SftMinterData;

    /// View method to calculate the deterministic AccountId of the
    /// wallet-contract for a given owner.
    fn sft_wallet_account_id_for(&self, owner_id: AccountId) -> AccountId;
}
```

**`sft_minter_data`**

- MUST be a view method (no state mutation).
- MUST return an `SftMinterData` struct containing `sft_wallet_code` and `total_supply`.

**`sft_wallet_account_id_for`**

- MUST be a view method.
- MUST return the deterministic `AccountId` derived from `StateInit` containing the wallet code and initial state for the given `owner_id`.
- The derivation MUST match: `StateInit::V1 { code: self.sft_wallet_code, data: SftWalletData::init_state(owner_id, current_account_id) }.derive_account_id()`.

#### `ShardedFungibleTokenBurner` (OPTIONAL)

Minter-contracts MAY implement burning support.

```rust
pub trait ShardedFungibleTokenBurner: ShardedFungibleTokenMinter {
    /// Called by a wallet-contract when an owner burns tokens.
    /// Returns the amount of tokens successfully burned.
    /// `amount - returned_value` will be refunded to the sender's wallet.
    ///
    /// MUST be #[payable] and require at least 1 yoctoNEAR attached.
    fn sft_on_burn(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128>;
}
```

**`sft_on_burn`**

- MUST require at least 1 yoctoNEAR attached deposit.
- MUST verify that the caller is the wallet-contract corresponding to `sender_id` (by deriving the expected wallet `AccountId`).
- MUST return the amount of tokens successfully burned as `U128`.
- If the call fails or returns a partial amount, the wallet-contract MUST refund `amount - used_amount` back to the sender's balance.

### Wallet-Contract Interface

#### `ShardedFungibleTokenWallet`

Every wallet-contract MUST implement this interface.

```rust
pub trait ShardedFungibleTokenWallet {
    /// View method returning all wallet data.
    fn sft_wallet_data(self) -> SftWalletData;

    /// Transfer `amount` tokens to `receiver_id`.
    ///
    /// MUST be #[payable] and require at least 1 yoctoNEAR attached.
    fn sft_send(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        notify: Option<TransferNotification>,
    ) -> PromiseOrValue<U128>;

    /// Receive tokens from the minter or a peer wallet-contract.
    ///
    /// MUST be #[payable] and require at least 1 yoctoNEAR attached.
    fn sft_receive(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        memo: Option<String>,
        notify: Option<TransferNotification>,
    ) -> PromiseOrValue<U128>;

    /// Burn `amount` tokens and notify the minter via `sft_on_burn()`.
    ///
    /// MUST be #[payable] and require at least 1 yoctoNEAR attached.
    fn sft_burn(
        &mut self,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128>;
}
```

**`sft_wallet_data`**

- MUST be a view method.
- MUST return `SftWalletData` with `minter_id`, `owner_id`, and `balance`.

**`sft_send`**

- MUST require at least 1 yoctoNEAR attached deposit.
- MUST verify that the caller is the `owner_id` of this wallet.
- MUST NOT allow self-transfers (where `receiver_id == owner_id`).
- MUST subtract `amount` from the sender's balance before making any cross-contract calls.
- MUST deploy and initialize the receiver's wallet-contract via `StateInit` if it doesn't already exist.
- MUST call `sft_receive()` on the receiver's deterministically-derived wallet-contract, passing `owner_id` as `sender_id`.
- If `notify` is set, the receiver's wallet-contract will call `sft_on_receive()` on the receiver's `owner_id`.
- MUST schedule a `sft_resolve_transfer` callback to handle partial refunds based on the result.
- Returns the amount actually used (i.e., successfully transferred).

**`sft_receive`**

- MUST require at least 1 yoctoNEAR attached deposit.
- MUST verify the caller is either:
  - The `minter_id` (for minting operations), OR
  - A valid peer wallet-contract (by deriving the expected wallet `AccountId` for `sender_id` and comparing with `predecessor_account_id`).
- MUST add `amount` to the receiver's balance.
- If `notify` is set, MUST call `sft_on_receive()` on `owner_id`, optionally deploying the owner's contract via `notify.state_init` if provided.
- If `notify` is set, MUST schedule a `sft_resolve_transfer` callback to handle refunds for unused tokens.
- Returns the amount actually used.

**`sft_burn`**

- MUST require at least 1 yoctoNEAR attached deposit.
- MUST verify that the caller is the `owner_id`.
- MUST subtract `amount` from the balance.
- MUST call `sft_on_burn()` on the `minter_id`, forwarding the attached deposit.
- MUST schedule a `sft_resolve_transfer` callback: if the minter does not support burning or returns a partial amount, the unburned tokens MUST be refunded to the sender's balance.
- Returns the amount actually burned.

**`sft_resolve_transfer` (internal callback)**

- MUST be a `#[private]` callback.
- MUST read the result of the preceding cross-contract call.
- For outgoing transfers (send/burn): MUST refund `amount - used_amount` back to the sender's balance.
- For incoming notification results: MUST refund `amount - used_amount` back to the sender by subtracting from the receiver's balance.
- MUST handle promise failures gracefully by treating them as full refunds.
- MUST cap `used_amount` at `amount` to prevent a faulty receiver from claiming more tokens than transferred.

### Receiver Interface (OPTIONAL)

Contracts wishing to react to incoming token transfers MUST implement:

```rust
pub trait ShardedFungibleTokenReceiver {
    /// Called by the wallet-contract upon receiving tokens when `notify` is set.
    ///
    /// Returns the number of tokens used. `amount - used` will be refunded
    /// to the sender.
    ///
    /// Note: `amount` can be zero.
    ///
    /// MUST be #[payable] and require at least 1 yoctoNEAR attached.
    fn sft_on_receive(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128>;
}
```

**`sft_on_receive`**

- MUST require at least 1 yoctoNEAR attached deposit.
- `sender_id` is the original owner who initiated the transfer. **Warning**: Do not blindly trust `sender_id`; a malicious minter can propagate arbitrary sender values. Verify by either:
  - Passing `minter_id` in `msg` and verifying `predecessor_account_id` matches `StateInit::derive_account_id()` for the expected wallet.
  - Calling `sft_wallet_data()` on `predecessor_account_id` and extracting `minter_id`.
- MUST return the number of tokens actually used as `U128`. Unused tokens (`amount - used`) are refunded to the sender's wallet.

### Governed Wallet Extension (OPTIONAL)

Minter-contracts MAY deploy a governed variant of the wallet-contract that allows the minter to control transfer permissions per-wallet.

```rust
pub trait ShardedFungibleTokenWalletGoverned: ShardedFungibleTokenWallet {
    /// Set wallet status flags. Only callable by the minter-contract.
    ///
    /// MUST require exactly 1 yoctoNEAR attached.
    fn sft_wallet_set_status(&mut self, status: u8);
}
```

**Status flags** (bitmask):

| Bit | Flag                          | Effect                              |
|-----|-------------------------------|-------------------------------------|
| 0   | `OUTGOING_TRANSFERS_LOCKED`   | Prevents `sft_send` and `sft_burn`  |
| 1   | `INCOMING_TRANSFERS_LOCKED`   | Prevents `sft_receive`              |

When the governed feature is active:
- `sft_send` and `sft_burn` MUST check that the `OUTGOING_TRANSFERS_LOCKED` flag is not set before proceeding (unless the caller is the minter itself).
- `sft_receive` MUST check that the `INCOMING_TRANSFERS_LOCKED` flag is not set.
- The minter-contract MAY call `sft_send` on a wallet directly to perform forced transfers, bypassing the owner check.

### Wallet-Contract Deterministic Address Derivation

Wallet-contract account IDs are derived deterministically using [NEP-616]:

```
StateInit::V1 {
    code: <globally deployed wallet contract code>,
    data: {
        b"": borsh(SftWalletData { minter_id, owner_id, balance: 0 })
    }
}
```

The resulting `AccountId` is: `"0s" ++ hex(keccak256(borsh(state_init))[12..32])`.

This allows anyone to:
1. **Compute** a wallet address offline from the minter's `sft_wallet_code` and any `owner_id`.
2. **Verify** that a calling wallet-contract is authentic by re-deriving its expected address.
3. **Send** tokens to a recipient before their wallet is deployed — the wallet will be created automatically.

### FT-to-SFT Bridge (`Ft2Sft`)

To facilitate migration from [NEP-141] tokens, a bridge adapter is defined:

```rust
pub trait Ft2Sft:
    ShardedFungibleTokenMinter + ShardedFungibleTokenBurner + FungibleTokenReceiver
{
    /// Returns the AccountId of the wrapped NEP-141 token contract.
    fn ft_contract_id(self) -> AccountId;
}
```

**Minting (wrapping)**: The bridge receives NEP-141 tokens via `ft_on_transfer()`, increments `total_supply`, and calls `sft_receive()` on the recipient's wallet-contract. The `msg` parameter of `ft_on_transfer()` accepts a JSON `MintMessage`:

```json
{
  "receiver_id": "<optional, defaults to sender_id>",
  "memo": "<optional>",
  "notify": { "msg": "<string>", ... },
  "refund_to": "<optional>"
}
```

**Burning (unwrapping)**: The bridge implements `sft_on_burn()`, decrements `total_supply`, and calls `ft_transfer()` or `ft_transfer_call()` on the underlying NEP-141 contract. The `msg` parameter of `sft_burn()` accepts a JSON `BurnMessage`:

```json
{
  "receiver_id": "<optional, defaults to sender_id>",
  "memo": "<optional>",
  "msg": "<if set, uses ft_transfer_call instead of ft_transfer>",
  "storage_deposit": "<optional NearToken amount for NEP-141 storage registration>",
  "refund_to": "<optional>"
}
```

### Events

Events follow the [NEP-636 event standard](https://github.com/nicechute/NEPs/blob/master/neps/nep-0636.md) with `standard = "nep636"`:

```json
{
  "standard": "nep636",
  "version": "1.0.0",
  "event": "<event_name>",
  "data": [...]
}
```

#### `sft_mint`

Emitted by the minter-contract when new tokens are minted.

```json
{
  "event": "sft_mint",
  "data": [{
    "owner_id": "<AccountId>",
    "amount": "<u128 as string>",
    "memo": "<optional string>"
  }]
}
```

#### `sft_send`

Emitted by the sender's wallet-contract when tokens are sent.

```json
{
  "event": "sft_send",
  "data": [{
    "receiver_id": "<AccountId>",
    "amount": "<u128 as string>",
    "memo": "<optional string>"
  }]
}
```

#### `sft_receive`

Emitted by the receiver's wallet-contract when tokens are received.

```json
{
  "event": "sft_receive",
  "data": [{
    "sender_id": "<AccountId>",
    "amount": "<u128 as string>",
    "memo": "<optional string>"
  }]
}
```

#### `sft_burn`

Emitted by the minter-contract when tokens are burned.

```json
{
  "event": "sft_burn",
  "data": [{
    "owner_id": "<AccountId>",
    "amount": "<u128 as string>",
    "memo": "<optional string>"
  }]
}
```

**Note on indexing**: Unlike centralized token standards, transfer events are split across sender and receiver wallet-contracts. Indexers MUST track cross-contract function calls (`sft_send`, `sft_receive`, `sft_burn`) and their receipt statuses to fully reconstruct transfer histories. Events emitted by wallet-contracts that are deployed as part of the same transaction provide additional confirmation but are not the sole source of truth.

### Transfer Flow

A standard transfer from Alice to Bob proceeds as follows:

```
Alice (owner)                 Alice's Wallet              Bob's Wallet              Bob (owner)
     │                              │                          │                        │
     │  sft_send(bob, 100, notify)  │                          │                        │
     │ ────────────────────────────► │                          │                        │
     │                              │  StateInit + sft_receive │                        │
     │                              │ ────────────────────────► │                        │
     │                              │                          │  sft_on_receive(...)   │
     │                              │                          │ ──────────────────────► │
     │                              │                          │       used_amount      │
     │                              │                          │ ◄────────────────────── │
     │                              │       used_amount        │                        │
     │                              │ ◄──────────────────────── │                        │
     │                              │                          │                        │
     │                              │ sft_resolve_transfer     │                        │
     │                              │ (refund if partial)      │                        │
     │       used_amount            │                          │                        │
     │ ◄──────────────────────────── │                          │                        │
```

1. Alice calls `sft_send(bob, 100, ...)` on her wallet-contract with at least 1 yoctoNEAR.
2. The wallet subtracts 100 from Alice's balance.
3. The wallet deploys Bob's wallet (if needed) via `StateInit` and calls `sft_receive()`.
4. Bob's wallet adds 100 to Bob's balance.
5. If `notify` was set, Bob's wallet calls `sft_on_receive()` on Bob's owner account.
6. Bob (or his contract) returns `used_amount`.
7. Alice's wallet's `sft_resolve_transfer` refunds `100 - used_amount` if partial.

### Gas Requirements

- `sft_receive` minimum gas: 5 TGas
- `sft_resolve_transfer` gas: 5 TGas
- Remaining gas SHOULD be forwarded to the main cross-contract call via `with_unused_gas_weight(1)`.
- Resolution callbacks SHOULD NOT receive leftover gas (`with_unused_gas_weight(0)`).

### Storage Requirements

Wallet-contracts MUST fit within NEAR's Zero Balance Account limits (< 770 bytes total including code and state), so no storage staking deposit is required. The wallet code is globally deployed and referenced by `GlobalContractId`, so each wallet account only stores:

- Account metadata: ~100 bytes
- Wallet state (`SftWalletData`): ~130-150 bytes (depending on `AccountId` lengths)

This totals well under the 770-byte ZBA threshold.

## Reference Implementation

A complete reference implementation is available in the [`near-sdk-rs`](https://github.com/near/near-sdk-rs) repository:

- **Wallet-contract**: [`examples/sharded-fungible-token/wallet/src/lib.rs`](https://github.com/near/near-sdk-rs/blob/master/examples/sharded-fungible-token/wallet/src/lib.rs)
- **FT-to-SFT bridge (minter)**: [`examples/sharded-fungible-token/ft2sft/src/lib.rs`](https://github.com/near/near-sdk-rs/blob/master/examples/sharded-fungible-token/ft2sft/src/lib.rs)
- **Standard library interfaces**: [`near-contract-standards/src/sharded_fungible_token/`](https://github.com/near/near-sdk-rs/tree/master/near-contract-standards/src/sharded_fungible_token)

The reference wallet-contract implementation supports an optional `governed` feature flag that enables minter-controlled transfer restrictions.

## Security Implications

### Caller Verification

Wallet-contracts verify callers by deriving the expected `AccountId` from `StateInit` and comparing it with `predecessor_account_id`. This ensures that only the authentic wallet-contract for a given `(owner_id, minter_id)` pair can call `sft_receive()`, preventing unauthorized minting.

### Sender Identity in Notifications

The `sender_id` passed to `sft_on_receive()` originates from the sender's wallet-contract and is propagated through `sft_receive()`. A malicious minter could fabricate `sender_id` values when calling `sft_receive()` directly. Receivers MUST verify the minter's identity by either:
1. Deriving the expected wallet `AccountId` from known `minter_id` and `sender_id`, or
2. Calling `sft_wallet_data()` on `predecessor_account_id`.

### Minimum Deposit Requirement

All mutating methods require at least 1 yoctoNEAR attached. This prevents key-based access from being exploited and aligns with established NEAR security practices.

### Balance Overflow/Underflow Protection

All arithmetic on balances and total supply MUST use checked operations. Overflow MUST cause the transaction to panic rather than silently wrap.

### Promise Resolution Safety

`sft_resolve_transfer` MUST cap `used_amount` at `amount` to prevent a faulty or malicious receiver from claiming more tokens than were actually transferred. Failed promises MUST be treated as zero usage (full refund to sender).

### Refund Routing

When transfers involve `StateInit` deployment, refunds for excess deposits are routed to `refund_to` (set via `env::promise_set_refund_to()`) rather than to the wallet-contract address. This ensures users recover their funds even when intermediate wallet-contracts are involved.

## Alternatives

### Centralized Token Contract (NEP-141)

The existing [NEP-141] standard stores all balances in a single contract. While simpler, it cannot scale beyond a single shard's throughput and suffers from unbounded state growth. The SFT standard is designed to complement, not replace, [NEP-141] — the `Ft2Sft` bridge provides seamless interoperability.

### Sub-account Based Sharding

An alternative approach would use named sub-accounts (e.g., `alice.token.near`) instead of deterministic accounts. However, this would:
- Tie tokens to specific top-level account namespaces.
- Prevent global wallet code reuse across different tokens.
- Require different deployment and permission models.

### NEP-605 Sharded Contexts

The earlier [NEP-605] proposal introduced "sharded contexts" with isolation boundaries between sharded and non-sharded contracts. [NEP-616] superseded this approach by enabling unrestricted interaction between deterministic and traditional accounts, which this standard leverages. The SFT standard benefits from the simpler, more composable model of [NEP-616].

## Future Possibilities

- **Multi-token wallets**: Extending wallet-contracts to hold balances for multiple tokens simultaneously, reducing the number of deployed contracts.
- **Governed tokens at scale**: The optional governance extension enables compliance-oriented tokens (e.g., stablecoins) that can freeze individual wallets without affecting the broader system.
- **Sharded DEXes and DeFi**: Composing SFT tokens with sharded exchange contracts for fully parallelized trading engines.
- **Co-location optimization**: Using deterministic account ID prefixes to co-locate frequently interacting wallets on the same shard.
- **Metadata extension**: Adding a metadata trait to the minter-contract for token name, symbol, decimals, and icon — analogous to NEP-148 for NEP-141.
- **Batch operations**: Supporting batch sends within a single wallet to reduce the number of cross-contract calls.

## Consequences

### Positive

- Enables horizontal scaling of fungible token operations across NEAR shards.
- Eliminates single-contract state bottleneck for popular tokens.
- Zero storage staking cost for wallet deployment via ZBA.
- Full composability with existing NEAR accounts and contracts.
- Global wallet code reuse reduces total network storage consumption.
- Deterministic addresses enable offline wallet address computation and authenticity verification.

### Neutral

- Indexers must adapt to track cross-contract function calls rather than relying solely on events from a single contract.
- Wallet deployment adds a one-time gas cost to the first transfer to a new recipient.
- Token balances are distributed; aggregating total holdings requires querying the minter or individual wallets.

### Negative

- Increased complexity compared to [NEP-141]: transfers involve multiple cross-contract calls instead of a single contract call.
- Higher gas costs per transfer due to cross-contract call overhead (deploy + `sft_receive` + optional `sft_on_receive` + `sft_resolve_transfer`).
- Wallet-contract upgrades require careful multi-phase rollout since the code is globally shared (see [NEP-616] security considerations).

### Backwards Compatibility

This standard does not modify [NEP-141] or any existing standards. The `Ft2Sft` bridge provides a migration path: users wrap [NEP-141] tokens into SFT tokens for sharded operation and can unwrap them back at any time. Both standards can coexist on the same network.

## Unresolved Issues

1. **Exact gas constants**: The minimum gas values for `sft_receive` (5 TGas) and `sft_resolve_transfer` (5 TGas) are preliminary and need benchmarking under production conditions.
2. **Metadata standard**: This proposal does not define a metadata interface (name, symbol, decimals). A companion NEP or extension to this standard should address token metadata.
3. **Governed wallet data layout**: The `status` field for the governed extension is not yet included in the base `SftWalletData` serialization format. The exact encoding needs finalization.
4. **Wallet code upgrades**: The process for upgrading globally deployed wallet code and migrating existing wallets needs further specification, building on [NEP-616]'s multi-stage upgrade protocol.
5. **Storage cost model evolution**: As NEAR transitions from fixed storage staking to rental-based models, wallet storage economics may change.

## Changelog

### 1.0.0 — Initial Draft

- Core specification for minter-contract, wallet-contract, and receiver interfaces.
- Deterministic address derivation via [NEP-616] `StateInit`.
- Event definitions following NEP-636.
- FT-to-SFT bridge specification.
- Optional governed wallet extension.

## Copyright

Copyright and related rights are waived via [CC0](https://creativecommons.org/publicdomain/zero/1.0/).

[NEP-141]: https://github.com/near/NEPs/blob/master/neps/nep-0141.md
[NEP-616]: https://github.com/near/NEPs/blob/master/neps/nep-0616.md
[NEP-605]: https://github.com/nicechute/NEPs/blob/master/neps/nep-0605.md
