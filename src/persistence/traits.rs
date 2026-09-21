//! Storage traits.
//!
//! **P1-8 fix**: the trait now requires optimistic concurrency control.
//! `put_with_version` rejects writes where the expected version does not
//! match the persisted version, returning [`Error::StateConflict`].
//! This guarantees that two concurrent evaluations of the same account
//! (e.g. a retried tick and a live tick arriving close together) can't
//! silently clobber each other's state.
//!
//! **P1-9 fix**: every read takes a `tenant_id` so cross-tenant data
//! leakage is impossible at the storage layer, not just at the API layer.

use crate::core::account::Account;
use crate::core::ids::{AccountId, PositionId};
use crate::core::position::Position;
use crate::core::trade::Trade;
use crate::core::Error;
use crate::tenant::TenantId;

/// Account + position + trade storage.
pub trait AccountStore: Send + Sync {
    /// Reads an account, scoped to a specific tenant. Returns `None` if
    /// the account doesn't exist OR if it belongs to a different tenant.
    /// This is the *only* read path in multi-tenant deployments.
    fn get_for_tenant(&self, tenant_id: TenantId, id: AccountId) -> Result<Option<Account>, Error>;

    /// Bootstrap read — fetches an account without tenant scoping.
    ///
    /// Intended *only* for internal-API endpoints where the caller needs
    /// the account's own `tenant_id` before it can issue a scoped read
    /// (e.g. `Pipeline::process`). Production code that already knows
    /// the tenant MUST use `get_for_tenant`.
    fn get(&self, id: AccountId) -> Result<Option<Account>, Error>;

    /// Writes an account without optimistic-concurrency checking. Last
    /// write wins. Use [`put_with_version`](Self::put_with_version) in
    /// any code path that requires concurrent-safety.
    fn put(&self, account: Account) -> Result<(), Error>;

    /// **P1-8 fix**: writes an account with optimistic-concurrency checking.
    /// Returns [`Error::StateConflict`] if the persisted version does not
    /// match `expected_version`. The caller must re-read, re-evaluate,
    /// and retry on conflict.
    fn put_with_version(&self, account: Account, expected_version: u64) -> Result<(), Error>;

    /// Deletes an account.
    fn delete(&self, id: AccountId) -> Result<(), Error>;

    /// Returns all open positions for an account.
    fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error>;

    /// Adds a position.
    fn add_position(&self, position: Position) -> Result<(), Error>;

    /// Updates a position.
    fn update_position(&self, position: Position) -> Result<(), Error>;

    /// Closes a position by id.
    fn close_position(&self, position_id: PositionId) -> Result<(), Error>;

    /// Returns today's trades for an account.
    fn today_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error>;

    /// Returns all trades for an account.
    fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error>;

    /// Adds a trade.
    fn add_trade(&self, trade: Trade) -> Result<(), Error>;
}
