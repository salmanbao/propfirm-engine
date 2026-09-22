//! Postgres persistence implementation (§E.1).
//!
//! Provides `AccountStore` and `RulePackStore` backed by Postgres,
//! including optimistic concurrency control and tenant isolation.
//!
//! `PostgresStore::connect()` applies embedded migrations from
//! `./migrations` automatically, so a freshly connected store is
//! immediately usable without out-of-band schema setup.

#![cfg(feature = "postgres")]

use crate::core::account::Account;
use crate::core::ids::{AccountId, PositionId};
use crate::core::position::Position;
use crate::core::trade::Trade;
use crate::core::Error;
use crate::persistence::traits::AccountStore;
use crate::tenant::TenantId;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;

fn block_on_async<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::runtime::Runtime::new()
        .expect("create tokio runtime for postgres store")
        .block_on(future)
}

#[derive(Clone)]
pub struct PostgresStore {
    pool: Arc<PgPool>,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self, Error> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Error::Persistence(e.to_string()))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| Error::Persistence(e.to_string()))?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }
}

impl AccountStore for PostgresStore {
    fn get_for_tenant(&self, tenant_id: TenantId, id: AccountId) -> Result<Option<Account>, Error> {
        let row = block_on_async(async {
            sqlx::query_as::<_, AccountRow>(
                r#"
                SELECT id, tenant_id, account_type, status, challenge_id, plan,
                       initial_balance, balance, equity, peak_equity, peak_balance,
                       started_at, deadline, day_start_balance, trading_day_index,
                       active_trading_days, day_counted_today, today_realized_pnl,
                       total_realized_pnl, total_commissions, total_swaps,
                       largest_day_profit, largest_day_loss, sum_positive_days_profit,
                       day_start_equity, target_reached_at, target_reached_on_day,
                       version, last_tick_ts, last_trade_at, payout_count,
                       balance_at_last_payout, last_payout_at, created_at, updated_at
                  FROM accounts
                 WHERE id = $1 AND tenant_id = $2
                "#,
            )
            .bind(id.0)
            .bind(tenant_id.0)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(row.map(|r| r.into_account()))
    }

    fn get(&self, id: AccountId) -> Result<Option<Account>, Error> {
        let row = block_on_async(async {
            sqlx::query_as::<_, AccountRow>(
                r#"
                SELECT id, tenant_id, account_type, status, challenge_id, plan,
                       initial_balance, balance, equity, peak_equity, peak_balance,
                       started_at, deadline, day_start_balance, trading_day_index,
                       active_trading_days, day_counted_today, today_realized_pnl,
                       total_realized_pnl, total_commissions, total_swaps,
                       largest_day_profit, largest_day_loss, sum_positive_days_profit,
                       day_start_equity, target_reached_at, target_reached_on_day,
                       version, last_tick_ts, last_trade_at, payout_count,
                       balance_at_last_payout, last_payout_at, created_at, updated_at
                  FROM accounts
                 WHERE id = $1
                "#,
            )
            .bind(id.0)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(row.map(|r| r.into_account()))
    }

    fn put(&self, account: Account) -> Result<(), Error> {
        let row = AccountRow::from_account(&account);
        block_on_async(async {
            sqlx::query(
                r#"
                INSERT INTO accounts
                  (id, tenant_id, account_type, status, challenge_id, plan,
                   initial_balance, balance, equity, peak_equity, peak_balance,
                   started_at, deadline, day_start_balance, trading_day_index,
                   active_trading_days, day_counted_today, today_realized_pnl,
                   total_realized_pnl, total_commissions, total_swaps,
                   largest_day_profit, largest_day_loss, sum_positive_days_profit,
                   day_start_equity, target_reached_at, target_reached_on_day,
                   version, last_tick_ts, last_trade_at, payout_count,
                   balance_at_last_payout, last_payout_at, created_at, updated_at)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,
                        $19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35)
                ON CONFLICT (id) DO UPDATE SET
                   tenant_id = EXCLUDED.tenant_id,
                   account_type = EXCLUDED.account_type,
                   status = EXCLUDED.status,
                   challenge_id = EXCLUDED.challenge_id,
                   plan = EXCLUDED.plan,
                   initial_balance = EXCLUDED.initial_balance,
                   balance = EXCLUDED.balance,
                   equity = EXCLUDED.equity,
                   peak_equity = EXCLUDED.peak_equity,
                   peak_balance = EXCLUDED.peak_balance,
                   started_at = EXCLUDED.started_at,
                   deadline = EXCLUDED.deadline,
                   day_start_balance = EXCLUDED.day_start_balance,
                   trading_day_index = EXCLUDED.trading_day_index,
                   active_trading_days = EXCLUDED.active_trading_days,
                   day_counted_today = EXCLUDED.day_counted_today,
                   today_realized_pnl = EXCLUDED.today_realized_pnl,
                   total_realized_pnl = EXCLUDED.total_realized_pnl,
                   total_commissions = EXCLUDED.total_commissions,
                   total_swaps = EXCLUDED.total_swaps,
                   largest_day_profit = EXCLUDED.largest_day_profit,
                   largest_day_loss = EXCLUDED.largest_day_loss,
                   sum_positive_days_profit = EXCLUDED.sum_positive_days_profit,
                   day_start_equity = EXCLUDED.day_start_equity,
                   target_reached_at = EXCLUDED.target_reached_at,
                   target_reached_on_day = EXCLUDED.target_reached_on_day,
                   version = accounts.version + 1,
                   last_tick_ts = EXCLUDED.last_tick_ts,
                   last_trade_at = EXCLUDED.last_trade_at,
                   payout_count = EXCLUDED.payout_count,
                   balance_at_last_payout = EXCLUDED.balance_at_last_payout,
                   last_payout_at = EXCLUDED.last_payout_at,
                   updated_at = NOW()
                "#,
            )
            .bind(row.id)
            .bind(row.tenant_id)
            .bind(row.account_type)
            .bind(row.status)
            .bind(row.challenge_id)
            .bind(row.plan)
            .bind(row.initial_balance)
            .bind(row.balance)
            .bind(row.equity)
            .bind(row.peak_equity)
            .bind(row.peak_balance)
            .bind(row.started_at)
            .bind(row.deadline)
            .bind(row.day_start_balance)
            .bind(row.trading_day_index)
            .bind(row.active_trading_days)
            .bind(row.day_counted_today)
            .bind(row.today_realized_pnl)
            .bind(row.total_realized_pnl)
            .bind(row.total_commissions)
            .bind(row.total_swaps)
            .bind(row.largest_day_profit)
            .bind(row.largest_day_loss)
            .bind(row.sum_positive_days_profit)
            .bind(row.day_start_equity)
            .bind(row.target_reached_at)
            .bind(row.target_reached_on_day)
            .bind(row.version)
            .bind(row.last_tick_ts)
            .bind(row.last_trade_at)
            .bind(row.payout_count)
            .bind(row.balance_at_last_payout)
            .bind(row.last_payout_at)
            .bind(row.created_at)
            .bind(row.updated_at)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(())
    }

    fn put_with_version(&self, account: Account, expected_version: u64) -> Result<(), Error> {
        let row = AccountRow::from_account(&account);
        let result = block_on_async(async {
            sqlx::query(
                r#"
                UPDATE accounts
                   SET status = $2,
                       balance = $3,
                       equity = $4,
                       peak_equity = $5,
                       peak_balance = $6,
                       day_start_balance = $7,
                       trading_day_index = $8,
                       active_trading_days = $9,
                       day_counted_today = $10,
                       today_realized_pnl = $11,
                       total_realized_pnl = $12,
                       total_commissions = $13,
                       total_swaps = $14,
                       largest_day_profit = $15,
                       largest_day_loss = $16,
                       sum_positive_days_profit = $17,
                       day_start_equity = $18,
                       target_reached_at = $19,
                       target_reached_on_day = $20,
                       last_tick_ts = $21,
                       last_trade_at = $22,
                       payout_count = $23,
                       balance_at_last_payout = $24,
                       last_payout_at = $25,
                       version = version + 1,
                       updated_at = NOW()
                 WHERE id = $1 AND version = $26
                "#,
            )
            .bind(row.id)
            .bind(row.status)
            .bind(row.balance)
            .bind(row.equity)
            .bind(row.peak_equity)
            .bind(row.peak_balance)
            .bind(row.day_start_balance)
            .bind(row.trading_day_index)
            .bind(row.active_trading_days)
            .bind(row.day_counted_today)
            .bind(row.today_realized_pnl)
            .bind(row.total_realized_pnl)
            .bind(row.total_commissions)
            .bind(row.total_swaps)
            .bind(row.largest_day_profit)
            .bind(row.largest_day_loss)
            .bind(row.sum_positive_days_profit)
            .bind(row.day_start_equity)
            .bind(row.target_reached_at)
            .bind(row.target_reached_on_day)
            .bind(row.last_tick_ts)
            .bind(row.last_trade_at)
            .bind(row.payout_count)
            .bind(row.balance_at_last_payout)
            .bind(row.last_payout_at)
            .bind(expected_version as i64)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(Error::StateConflict(
                format!("account {}", row.id),
                expected_version,
                row.version.try_into().unwrap(),
            ))
        }
    }

    fn delete(&self, id: AccountId) -> Result<(), Error> {
        block_on_async(async {
            sqlx::query("DELETE FROM accounts WHERE id = $1")
                .bind(id.0)
                .execute(self.pool.as_ref())
                .await
                .map_err(|e| Error::Persistence(e.to_string()))
        })?;
        Ok(())
    }

    fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error> {
        let rows = block_on_async(async {
            sqlx::query_as::<_, PositionRow>(
                r#"
                SELECT id, account_id, symbol, side, opened_quantity, open_quantity, avg_entry_price,
                       status, opened_at, closed_at, closed_price, realized_pnl,
                       swap, commission, stop_loss, take_profit, magic, comment, metadata, created_at, updated_at
                  FROM positions
                 WHERE account_id = $1 AND status = 'Open'
                "#,
            )
            .bind(id.0)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    fn add_position(&self, position: Position) -> Result<(), Error> {
        let row = PositionRow::from(position);
        block_on_async(async {
            sqlx::query(
                r#"
                INSERT INTO positions
                  (id, account_id, symbol, side, opened_quantity, open_quantity, avg_entry_price,
                   status, opened_at, closed_at, realized_pnl,
                   swap, commission, stop_loss, take_profit, magic, comment,
                   created_at, updated_at)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
                "#,
            )
            .bind(row.id)
            .bind(row.account_id)
            .bind(row.symbol)
            .bind(row.side)
            .bind(row.opened_quantity)
            .bind(row.open_quantity)
            .bind(row.avg_entry_price)
            .bind(row.status)
            .bind(row.opened_at)
            .bind(row.closed_at)
            .bind(row.realized_pnl)
            .bind(row.swap)
            .bind(row.commission)
            .bind(row.stop_loss)
            .bind(row.take_profit)
            .bind(row.magic)
            .bind(row.comment)
            .bind(row.created_at)
            .bind(row.updated_at)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(())
    }

    fn update_position(&self, position: Position) -> Result<(), Error> {
        let row = PositionRow::from(position);
        block_on_async(async {
            sqlx::query(
                r#"
                UPDATE positions
                   SET account_id = $2,
                       symbol = $3,
                       side = $4,
                       opened_quantity = $5,
                       open_quantity = $6,
                       avg_entry_price = $7,
                       status = $8,
                       opened_at = $9,
                       closed_at = $10,
                       realized_pnl = $11,
                       swap = $12,
                       commission = $13,
                       stop_loss = $14,
                       take_profit = $15,
                       magic = $16,
                       comment = $17,
                       updated_at = NOW()
                 WHERE id = $1
                "#,
            )
            .bind(row.id)
            .bind(row.account_id)
            .bind(row.symbol)
            .bind(row.side)
            .bind(row.opened_quantity)
            .bind(row.open_quantity)
            .bind(row.avg_entry_price)
            .bind(row.status)
            .bind(row.opened_at)
            .bind(row.closed_at)
            .bind(row.realized_pnl)
            .bind(row.swap)
            .bind(row.commission)
            .bind(row.stop_loss)
            .bind(row.take_profit)
            .bind(row.magic)
            .bind(row.comment)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(())
    }

    fn close_position(&self, position_id: PositionId) -> Result<(), Error> {
        let now = chrono::Utc::now();
        block_on_async(async {
            sqlx::query(
                r#"
                UPDATE positions
                   SET status = 'closed',
                       closed_at = $2,
                       updated_at = NOW()
                 WHERE id = $1
                "#,
            )
            .bind(position_id.0)
            .bind(now)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(())
    }

    fn today_trades_since(
        &self,
        id: AccountId,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Trade>, Error> {
        let rows = block_on_async(async {
            sqlx::query_as::<_, TradeRow>(
                r#"
                SELECT id, account_id, symbol, side, trade_side, price, quantity,
                       commission, swap, executed_at, position_id, realized_pnl,
                       exit_price, closed_quantity, entry_price, comment, created_at
                  FROM trades
                 WHERE account_id = $1 AND executed_at >= $2
                 ORDER BY executed_at ASC
                "#,
            )
            .bind(id.0)
            .bind(since)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error> {
        let rows = block_on_async(async {
            sqlx::query_as::<_, TradeRow>(
                r#"
                SELECT id, account_id, symbol, side, trade_side, price, quantity,
                       commission, swap, executed_at, position_id, realized_pnl,
                       exit_price, closed_quantity, entry_price, comment, created_at
                  FROM trades
                 WHERE account_id = $1
                 ORDER BY executed_at ASC
                "#,
            )
            .bind(id.0)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    fn add_trade(&self, trade: Trade) -> Result<(), Error> {
        let row = TradeRow::from(trade);
        block_on_async(async {
            sqlx::query(
                r#"
                INSERT INTO trades
                  (id, account_id, symbol, side, trade_side, price, quantity,
                   commission, swap, executed_at, position_id, realized_pnl,
                   exit_price, closed_quantity, entry_price, comment, created_at)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
                "#,
            )
            .bind(row.id)
            .bind(row.account_id)
            .bind(row.symbol)
            .bind(row.side)
            .bind(row.trade_side)
            .bind(row.price)
            .bind(row.quantity)
            .bind(row.commission)
            .bind(row.swap)
            .bind(row.executed_at)
            .bind(row.position_id)
            .bind(row.realized_pnl)
            .bind(row.exit_price)
            .bind(row.closed_quantity)
            .bind(row.entry_price)
            .bind(row.comment)
            .bind(row.created_at)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Error::Persistence(e.to_string()))
        })?;

        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: uuid::Uuid,
    tenant_id: uuid::Uuid,
    account_type: String,
    status: String,
    challenge_id: uuid::Uuid,
    plan: serde_json::Value,
    initial_balance: rust_decimal::Decimal,
    balance: rust_decimal::Decimal,
    equity: rust_decimal::Decimal,
    peak_equity: rust_decimal::Decimal,
    peak_balance: rust_decimal::Decimal,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    day_start_balance: rust_decimal::Decimal,
    trading_day_index: i32,
    active_trading_days: i32,
    day_counted_today: bool,
    today_realized_pnl: rust_decimal::Decimal,
    total_realized_pnl: rust_decimal::Decimal,
    total_commissions: rust_decimal::Decimal,
    total_swaps: rust_decimal::Decimal,
    largest_day_profit: rust_decimal::Decimal,
    largest_day_loss: rust_decimal::Decimal,
    sum_positive_days_profit: rust_decimal::Decimal,
    day_start_equity: rust_decimal::Decimal,
    target_reached_at: Option<chrono::DateTime<chrono::Utc>>,
    target_reached_on_day: Option<i32>,
    version: i64,
    last_tick_ts: Option<chrono::DateTime<chrono::Utc>>,
    last_trade_at: Option<chrono::DateTime<chrono::Utc>>,
    payout_count: i32,
    balance_at_last_payout: rust_decimal::Decimal,
    last_payout_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow)]
struct PositionRow {
    id: uuid::Uuid,
    account_id: uuid::Uuid,
    symbol: String,
    side: String,
    opened_quantity: rust_decimal::Decimal,
    open_quantity: rust_decimal::Decimal,
    avg_entry_price: rust_decimal::Decimal,
    status: String,
    opened_at: chrono::DateTime<chrono::Utc>,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    realized_pnl: rust_decimal::Decimal,
    commission: rust_decimal::Decimal,
    swap: rust_decimal::Decimal,
    stop_loss: Option<rust_decimal::Decimal>,
    take_profit: Option<rust_decimal::Decimal>,
    magic: Option<i64>,
    comment: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow)]
struct TradeRow {
    id: uuid::Uuid,
    account_id: uuid::Uuid,
    symbol: String,
    side: String,
    trade_side: String,
    price: rust_decimal::Decimal,
    quantity: rust_decimal::Decimal,
    commission: rust_decimal::Decimal,
    swap: rust_decimal::Decimal,
    executed_at: chrono::DateTime<chrono::Utc>,
    position_id: Option<uuid::Uuid>,
    realized_pnl: Option<rust_decimal::Decimal>,
    exit_price: Option<rust_decimal::Decimal>,
    closed_quantity: Option<rust_decimal::Decimal>,
    entry_price: Option<rust_decimal::Decimal>,
    comment: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl AccountRow {
    fn into_account(self) -> Account {
        let account_type = match self.account_type.as_str() {
            "Phase1" => crate::core::account::AccountType::Phase1,
            "Phase2" => crate::core::account::AccountType::Phase2,
            "Funded" => crate::core::account::AccountType::Funded,
            "Demo" => crate::core::account::AccountType::Demo,
            _ => crate::core::account::AccountType::Phase1,
        };
        let status = match self.status.as_str() {
            "Pending" => crate::core::account::AccountStatus::Pending,
            "Active" => crate::core::account::AccountStatus::Active,
            "TargetHitPending" => crate::core::account::AccountStatus::TargetHitPending,
            "Passed" => crate::core::account::AccountStatus::Passed,
            "Failed" => crate::core::account::AccountStatus::Failed,
            "Funded" => crate::core::account::AccountStatus::Funded,
            "PayoutPending" => crate::core::account::AccountStatus::PayoutPending,
            "Closed" => crate::core::account::AccountStatus::Closed,
            "EmergencyStopped" => crate::core::account::AccountStatus::EmergencyStopped,
            _ => crate::core::account::AccountStatus::Pending,
        };
        let plan: crate::config::plan::ChallengePlan =
            serde_json::from_value(self.plan).unwrap_or_default();
        Account {
            id: AccountId(self.id),
            account_type,
            status,
            challenge_id: crate::core::ids::ChallengeId(self.challenge_id),
            plan,
            tenant_id: TenantId(self.tenant_id),
            initial_balance: crate::core::types::Money(self.initial_balance),
            balance: crate::core::types::Money(self.balance),
            equity: crate::core::types::Money(self.equity),
            peak_equity: crate::core::types::Money(self.peak_equity),
            peak_balance: crate::core::types::Money(self.peak_balance),
            started_at: self.started_at,
            deadline: self.deadline,
            day_start_balance: crate::core::types::Money(self.day_start_balance),
            trading_day_index: self.trading_day_index as u32,
            active_trading_days: self.active_trading_days as u32,
            day_counted_today: self.day_counted_today,
            today_realized_pnl: crate::core::types::Money(self.today_realized_pnl),
            total_realized_pnl: crate::core::types::Money(self.total_realized_pnl),
            total_commissions: crate::core::types::Money(self.total_commissions),
            total_swaps: crate::core::types::Money(self.total_swaps),
            largest_day_profit: crate::core::types::Money(self.largest_day_profit),
            largest_day_loss: crate::core::types::Money(self.largest_day_loss),
            sum_positive_days_profit: crate::core::types::Money(self.sum_positive_days_profit),
            day_start_equity: crate::core::types::Money(self.day_start_equity),
            target_reached_at: self.target_reached_at,
            target_reached_on_day: self.target_reached_on_day.map(|v| v as u32),
            version: self.version as u64,
            last_tick_ts: self.last_tick_ts,
            last_trade_at: self.last_trade_at,
            payout_count: self.payout_count as u32,
            balance_at_last_payout: crate::core::types::Money(self.balance_at_last_payout),
            last_payout_at: self.last_payout_at,
            refund_used: false,
        }
    }

    fn from_account(account: &Account) -> Self {
        let account_type = match account.account_type {
            crate::core::account::AccountType::Phase1 => "Phase1",
            crate::core::account::AccountType::Phase2 => "Phase2",
            crate::core::account::AccountType::Funded => "Funded",
            crate::core::account::AccountType::Demo => "Demo",
        };
        let status = match account.status {
            crate::core::account::AccountStatus::Pending => "Pending",
            crate::core::account::AccountStatus::Active => "Active",
            crate::core::account::AccountStatus::TargetHitPending => "TargetHitPending",
            crate::core::account::AccountStatus::Passed => "Passed",
            crate::core::account::AccountStatus::Failed => "Failed",
            crate::core::account::AccountStatus::Funded => "Funded",
            crate::core::account::AccountStatus::PayoutPending => "PayoutPending",
            crate::core::account::AccountStatus::Closed => "Closed",
            crate::core::account::AccountStatus::EmergencyStopped => "EmergencyStopped",
        };
        Self {
            id: account.id.0,
            tenant_id: account.tenant_id.0,
            account_type: account_type.to_string(),
            status: status.to_string(),
            challenge_id: account.challenge_id.0,
            plan: serde_json::to_value(&account.plan).unwrap_or_default(),
            initial_balance: account.initial_balance.0,
            balance: account.balance.0,
            equity: account.equity.0,
            peak_equity: account.peak_equity.0,
            peak_balance: account.peak_balance.0,
            started_at: account.started_at,
            deadline: account.deadline,
            day_start_balance: account.day_start_balance.0,
            trading_day_index: account.trading_day_index as i32,
            active_trading_days: account.active_trading_days as i32,
            day_counted_today: account.day_counted_today,
            today_realized_pnl: account.today_realized_pnl.0,
            total_realized_pnl: account.total_realized_pnl.0,
            total_commissions: account.total_commissions.0,
            total_swaps: account.total_swaps.0,
            largest_day_profit: account.largest_day_profit.0,
            largest_day_loss: account.largest_day_loss.0,
            sum_positive_days_profit: account.sum_positive_days_profit.0,
            day_start_equity: account.day_start_equity.0,
            target_reached_at: account.target_reached_at,
            target_reached_on_day: account.target_reached_on_day.map(|v| v as i32),
            version: account.version as i64,
            last_tick_ts: account.last_tick_ts,
            last_trade_at: account.last_trade_at,
            payout_count: account.payout_count as i32,
            balance_at_last_payout: account.balance_at_last_payout.0,
            last_payout_at: account.last_payout_at,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}

impl From<PositionRow> for Position {
    fn from(row: PositionRow) -> Self {
        let side = match row.side.as_str() {
            "Long" => crate::core::position::PositionSide::Long,
            "Short" => crate::core::position::PositionSide::Short,
            _ => crate::core::position::PositionSide::Long,
        };
        let status = match row.status.as_str() {
            "Open" => crate::core::position::PositionStatus::Open,
            "Closed" => crate::core::position::PositionStatus::Closed,
            _ => crate::core::position::PositionStatus::Open,
        };
        Self {
            id: crate::core::ids::PositionId(row.id),
            account_id: AccountId(row.account_id),
            symbol: crate::core::types::Symbol(row.symbol),
            side,
            opened_at: row.opened_at,
            closed_at: row.closed_at,
            status,
            avg_entry_price: crate::core::types::Price(row.avg_entry_price),
            opened_quantity: crate::core::types::Quantity(row.opened_quantity),
            open_quantity: crate::core::types::Quantity(row.open_quantity),
            realized_pnl: crate::core::types::Money(row.realized_pnl),
            commission: crate::core::types::Money(row.commission),
            swap: crate::core::types::Money(row.swap),
            stop_loss: row.stop_loss.map(crate::core::types::Price),
            take_profit: row.take_profit.map(crate::core::types::Price),
            magic: row.magic.map(|v| v as u64),
            comment: row.comment,
        }
    }
}

impl From<Position> for PositionRow {
    fn from(position: Position) -> Self {
        let side = match position.side {
            crate::core::position::PositionSide::Long => "Long",
            crate::core::position::PositionSide::Short => "Short",
        };
        let status = match position.status {
            crate::core::position::PositionStatus::Open => "Open",
            crate::core::position::PositionStatus::Closed => "Closed",
            crate::core::position::PositionStatus::Liquidated => "Liquidated",
        };
        Self {
            id: position.id.0,
            account_id: position.account_id.0,
            symbol: position.symbol.to_string(),
            side: side.to_string(),
            opened_quantity: position.opened_quantity.0,
            open_quantity: position.open_quantity.0,
            avg_entry_price: position.avg_entry_price.0,
            status: status.to_string(),
            opened_at: position.opened_at,
            closed_at: position.closed_at,
            realized_pnl: position.realized_pnl.0,
            commission: position.commission.0,
            swap: position.swap.0,
            stop_loss: position.stop_loss.map(|p| p.0),
            take_profit: position.take_profit.map(|p| p.0),
            magic: position.magic.map(|v| v as i64),
            comment: position.comment,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}

impl From<TradeRow> for Trade {
    fn from(row: TradeRow) -> Self {
        let exit_info = if let (
            Some(position_id),
            Some(realized_pnl),
            Some(exit_price),
            Some(closed_quantity),
            Some(entry_price),
        ) = (
            row.position_id,
            row.realized_pnl,
            row.exit_price,
            row.closed_quantity,
            row.entry_price,
        ) {
            Some(crate::core::trade::TradeExit {
                position_id: crate::core::ids::PositionId(position_id),
                realized_pnl: crate::core::types::Money(realized_pnl),
                closed_quantity: crate::core::types::Quantity(closed_quantity),
                entry_price: crate::core::types::Price(entry_price),
                exit_price: crate::core::types::Price(exit_price),
            })
        } else {
            None
        };

        Self {
            id: crate::core::ids::TradeId(row.id),
            order_id: crate::core::ids::OrderId::new(),
            account_id: AccountId(row.account_id),
            symbol: crate::core::types::Symbol(row.symbol),
            side: match row.side.as_str() {
                "Buy" => crate::core::order::OrderSide::Buy,
                "Sell" => crate::core::order::OrderSide::Sell,
                _ => crate::core::order::OrderSide::Buy,
            },
            trade_side: match row.trade_side.as_str() {
                "Entry" => crate::core::trade::TradeSide::Entry,
                "Exit" => crate::core::trade::TradeSide::Exit,
                _ => crate::core::trade::TradeSide::Entry,
            },
            price: crate::core::types::Price(row.price),
            quantity: crate::core::types::Quantity(row.quantity),
            commission: crate::core::types::Money(row.commission),
            swap: crate::core::types::Money(row.swap),
            executed_at: row.executed_at,
            exit_info,
            comment: row.comment,
        }
    }
}

impl From<Trade> for TradeRow {
    fn from(trade: Trade) -> Self {
        let (position_id, realized_pnl, exit_price, closed_quantity, entry_price, comment) =
            if let Some(exit) = trade.exit_info {
                (
                    Some(exit.position_id.0),
                    Some(exit.realized_pnl.0),
                    Some(exit.exit_price.0),
                    Some(exit.closed_quantity.0),
                    Some(exit.entry_price.0),
                    trade.comment,
                )
            } else {
                (None, None, None, None, None, trade.comment)
            };

        Self {
            id: trade.id.0,
            account_id: trade.account_id.0,
            symbol: trade.symbol.to_string(),
            side: match trade.side {
                crate::core::order::OrderSide::Buy => "Buy".to_string(),
                crate::core::order::OrderSide::Sell => "Sell".to_string(),
            },
            trade_side: match trade.trade_side {
                crate::core::trade::TradeSide::Entry => "Entry".to_string(),
                crate::core::trade::TradeSide::Exit => "Exit".to_string(),
                crate::core::trade::TradeSide::Reverse => "Reverse".to_string(),
            },
            price: trade.price.0,
            quantity: trade.quantity.0,
            commission: trade.commission.0,
            swap: trade.swap.0,
            executed_at: trade.executed_at,
            position_id,
            realized_pnl,
            exit_price,
            closed_quantity,
            entry_price,
            comment,
            created_at: chrono::Utc::now(),
        }
    }
}
