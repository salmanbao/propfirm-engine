//! §E.1 Postgres persistence tests.
//!
//! These tests require a running Postgres instance and the `postgres`
//! feature flag. They are skipped automatically when the feature is
//! not enabled.

#[cfg(feature = "postgres")]
mod postgres_tests {
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::core::account::Account;
    use propfirm::core::ids::AccountId;
    use propfirm::core::position::{Position, PositionSide};
    use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
    use propfirm::persistence::postgres::PostgresStore;
    use propfirm::persistence::traits::AccountStore;

    #[test]
    fn postgres_put_and_get_roundtrip() {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:password@localhost:5432/propfirm_test".to_string()
        });

        let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
        let store = runtime
            .block_on(PostgresStore::connect(&database_url))
            .expect("failed to connect to postgres");

        let plan = ftmo_phase1();
        let account = Account::new(AccountId::new(), plan.clone())
            .start(chrono::Utc::now())
            .unwrap();

        store.put(account.clone()).unwrap();

        let fetched = store.get(account.id).unwrap().expect("account not found");
        assert_eq!(fetched.id, account.id);
        assert_eq!(fetched.status, account.status);
        assert_eq!(fetched.equity, account.equity);
    }

    #[test]
    fn postgres_open_positions_and_trades() {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:password@localhost:5432/propfirm_test".to_string()
        });

        let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
        let store = runtime
            .block_on(PostgresStore::connect(&database_url))
            .expect("failed to connect to postgres");

        let plan = ftmo_phase1();
        let account = Account::new(AccountId::new(), plan)
            .start(chrono::Utc::now())
            .unwrap();

        store.put(account.clone()).unwrap();

        let position = Position::open(
            account.id,
            Symbol::new("EURUSD"),
            PositionSide::Long,
            Price(dec!(1.0800)),
            Quantity(dec!(1)),
            chrono::Utc::now(),
            Money::ZERO,
            None,
            None,
            None,
            None,
        );

        store.add_position(position.clone()).unwrap();

        let open = store.open_positions(account.id).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, position.id);

        store.close_position(position.id).unwrap();

        let closed = store.open_positions(account.id).unwrap();
        assert!(
            closed.is_empty(),
            "closed position must not appear in open_positions"
        );
    }
}
