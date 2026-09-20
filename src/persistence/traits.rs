//! Storage traits.

use crate::core::account::Account;
use crate::core::ids::AccountId;
use crate::core::position::Position;
use crate::core::trade::Trade;
use crate::core::Error;

/// Account + position + trade storage.
pub trait AccountStore: Send + Sync {
    fn get(&self, id: AccountId) -> Result<Option<Account>, Error>;
    fn put(&self, account: Account) -> Result<(), Error>;
    fn delete(&self, id: AccountId) -> Result<(), Error>;

    fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error>;
    fn add_position(&self, position: Position) -> Result<(), Error>;
    fn update_position(&self, position: Position) -> Result<(), Error>;
    fn close_position(&self, position_id: crate::core::ids::PositionId) -> Result<(), Error>;

    fn today_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error>;
    fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error>;
    fn add_trade(&self, trade: Trade) -> Result<(), Error>;
}
