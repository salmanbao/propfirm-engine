//! CLI entry point.
//!
//! ## Modes
//!
//! - `propfirm-cli` (no args)  → run the built-in demo (start → order → tick → risk metrics).
//! - `propfirm-cli repl`       → interactive REPL: paste JSON request bodies, get verdicts.
//! - `propfirm-cli repl -f FILE` → read JSON requests from FILE, one per line, batch mode.
//!
//! ## REPL usage
//!
//! ```bash
//! cargo run --release --features tokio-cli --bin propfirm-cli repl
//! ```
//!
//! Then paste JSON request bodies (one per line):
//!
//! ```json
//! {"account_id": "...", "account_state": {...}, "bridge_tick": {...}}
//! ```
//!
//! The REPL responds with the decision kind, winning priority, input
//! hash, and any violations. Useful for debugging rule behavior
//! without running the full HTTP server.

use propfirm::config::presets::ftmo_phase1;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::notifications::log::LogNotifier;
use propfirm::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Subcommand dispatch.
    #[cfg(feature = "server")]
    if args.len() >= 2 && args[1] == "repl" {
        return run_repl(&args[2..]).await;
    }

    // Default: run the built-in demo.
    run_demo().await
}

/// Default mode — the built-in end-to-end demo.
async fn run_demo() -> anyhow::Result<()> {
    println!("=== Prop Firm Engine – CLI Demo ===\n");

    // 1. Build the challenge plan and account.
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    println!(
        "Account: id={} type={} phase={} initial={}",
        account.id, account.account_type, plan.phase, account.initial_balance
    );
    println!(
        "Plan: profit_target={} daily_dd={} max_dd={} min_days={:?} time_limit_days={:?}",
        plan.profit_target_pct,
        plan.max_daily_drawdown_pct,
        plan.max_total_drawdown_pct,
        plan.min_trading_days,
        plan.time_limit_days
    );

    // 2. Build the pipeline.
    let evaluator = Evaluator::new(&plan);
    let notifier = LogNotifier::new();
    let mut pipeline = Pipeline::new(evaluator, notifier);

    // 3. Start the account.
    let now = chrono::Utc::now();
    let result = pipeline
        .process(account.clone(), PipelineEvent::AccountStarted { at: now })
        .await?;
    println!(
        "\n[Started] decision={:?} events={}",
        result.snapshot.decision.kind,
        result.events.len()
    );

    // 4. Open a long EURUSD position.
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(1)),
        tif: TimeInForce::Ioc,
        stop_loss: Some(Price(dec!(1.05))),
        take_profit: Some(Price(dec!(1.10))),
        comment: Some("demo".into()),
        submitted_at: now,
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let result = pipeline
        .process(account.clone(), PipelineEvent::OrderSubmitted { order })
        .await?;
    println!(
        "[Order] decision={:?} passed={} violations={}",
        result.snapshot.decision.kind,
        result.result.passed(),
        result.result.violations().len()
    );

    // 5. A broker-reported tick comes in (positive move). P1-5: the engine
    //    does NOT recompute equity — it trusts the broker's number.
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0850)),
            ask: Price(dec!(1.0852)),
            ts: now,
        },
    );
    // In a real deployment this comes from the bridge/BRG module. Here we
    // synthesize a broker-reported equity that matches our expected value.
    let broker_equity = Money(dec!(10_200));
    let broker_balance = Money(dec!(10_000));
    let result = pipeline
        .process(
            account.clone(),
            PipelineEvent::Tick {
                tick,
                broker_equity,
                broker_balance,
            },
        )
        .await?;
    println!(
        "[Tick] equity={} balance={} daily_dd={}/{}",
        result.snapshot.account.equity,
        result.snapshot.account.balance,
        result.snapshot.account.daily_drawdown,
        account.daily_dd_limit()
    );

    // 6. Risk metrics computation.
    let equity_curve = vec![
        Money(dec!(10_000)),
        Money(dec!(10_200)),
        Money(dec!(10_150)),
        Money(dec!(10_300)),
    ];
    let risk = propfirm::risk::metrics::RiskMetrics::compute(
        &equity_curve,
        &[Money(dec!(100)), Money(dec!(-50)), Money(dec!(150))],
    );
    println!(
        "\n[Risk] sharpe={:.4} sortino={:.4} max_dd={:.4} profit_factor={:.4} win_rate={:.2}%",
        risk.sharpe,
        risk.sortino,
        risk.max_drawdown,
        risk.profit_factor,
        risk.win_rate * dec!(100)
    );

    println!("\nDone. Engine worked end-to-end. ✓");
    Ok(())
}

/// Interactive REPL mode — paste JSON request bodies, get verdicts.
///
/// Each line of input is one JSON request body — the same shape
/// `/internal/v1/evaluate` accepts. The REPL parses it, calls
/// `pure::evaluate`, and prints:
/// - decision kind (Pass, Warn, Fail, Liquidate, Emergency, ...)
/// - winning priority (numeric — shows which rule "won")
/// - input_hash (sha256 — for reproducibility)
/// - per-rule verdict table (so you can see why the engine
///   decided what it decided)
#[cfg(feature = "server")]
async fn run_repl(args: &[String]) -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader};

    // Parse args: --json for JSON output, -f FILE for batch mode.
    let json_mode = args.iter().any(|a| a == "--json" || a == "-j");

    // Parse optional `-f FILE` arg for batch mode.
    let mut input: Box<dyn BufRead> = if let Some(path) = args.get(1) {
        // Skip "-f" or "--json"
        let path = if path == "-f" {
            args.get(2).map(String::as_str).unwrap_or("-")
        } else if path == "--json" || path == "-j" {
            // Look for -f after --json
            if let Some(idx) = args.iter().position(|a| a == "-f") {
                args.get(idx + 1).map(String::as_str).unwrap_or("-")
            } else {
                // No -f, read from stdin.
                "stdin-marker"
            }
        } else {
            path.as_str()
        };
        if path == "stdin-marker" || path == "-" {
            Box::new(BufReader::new(std::io::stdin()))
        } else {
            let f = std::fs::File::open(path)
                .map_err(|e| anyhow::anyhow!("failed to open input file {path}: {e}"))?;
            Box::new(BufReader::new(f))
        }
    } else {
        Box::new(BufReader::new(std::io::stdin()))
    };

    if !json_mode {
        eprintln!("propfirm-cli repl — type a JSON request body per line, then Enter.");
        eprintln!("Each line is one `/internal/v1/evaluate` request.");
        eprintln!("Use --json / -j for JSON output (pipe to jq).");
        eprintln!("Press Ctrl+D (or empty line + Enter) to exit.\n");
    }

    let mut line = String::new();
    let mut line_no = 0usize;
    loop {
        line.clear();
        line_no += 1;
        let n = input.read_line(&mut line)?;
        if n == 0 {
            // EOF — exit.
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            // Comment line — skip.
            continue;
        }

        match process_one_repl_line(trimmed, json_mode).await {
            Ok(output) => println!("{output}"),
            Err(e) => {
                if json_mode {
                    eprintln!(r#"{{"error":"line {line_no}: {e}"}}"#);
                } else {
                    eprintln!("line {line_no}: ERROR — {e}\n");
                }
            }
        }
    }

    Ok(())
}

/// Parse one JSON request, run pure::evaluate, pretty-print the result.
#[cfg(feature = "server")]
async fn process_one_repl_line(json: &str, json_mode: bool) -> anyhow::Result<String> {
    use propfirm::api::handlers::InternalEvaluateRequest;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::ServerTime;
    use propfirm::pure::{self, EquitySource, EvaluateInputs};
    use propfirm::rulepack::RulePack;
    use propfirm::rules::context::RuleContextKind;
    use propfirm::rules::registry::RuleRegistry;
    use propfirm::tenant::TenantId;
    use std::str::FromStr;
    use uuid::Uuid;

    let req: InternalEvaluateRequest =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("failed to parse JSON: {e}"))?;

    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| anyhow::anyhow!("invalid account_id: {e}"))?,
    );
    let tenant_id = TenantId::from_str(
        req.account_state
            .as_ref()
            .map(|a| a.tenant_id.to_string())
            .as_deref()
            .unwrap_or("00000000-0000-0000-0000-000000000000"),
    )
    .map_err(|e| anyhow::anyhow!("invalid tenant_id: {e}"))?;

    let mut acc = req
        .account_state
        .ok_or_else(|| anyhow::anyhow!("account_state is required"))?;

    let (equity_source, _bridge_tick) = match req.bridge_tick {
        Some(ref bt) => (EquitySource::BrokerReported, Some(bt)),
        None => {
            let src = match req.equity_source.as_deref() {
                None | Some("estimated") => EquitySource::Estimated,
                Some(other) => EquitySource::parse(other)
                    .map_err(|e| anyhow::anyhow!("invalid equity_source: {e}"))?,
            };
            (src, None)
        }
    };

    let pack = RulePack::synthetic_from_plan(account_id, tenant_id, &acc.plan);
    let registry = RuleRegistry::with_default_rules_for_plan(&acc.plan);

    // Parse positions + trades.
    let mut positions = Vec::new();
    for p in req.open_positions.unwrap_or_default() {
        positions.push(
            p.into_domain(account_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }
    let mut trades = Vec::new();
    for t in req.today_trades.unwrap_or_default() {
        trades.push(
            t.into_domain(account_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }

    let server_time = if let Some(ref bt) = req.bridge_tick {
        ServerTime(
            chrono::DateTime::from_timestamp_millis(bt.payload.broker_time)
                .ok_or_else(|| anyhow::anyhow!("invalid broker_time"))?,
        )
    } else if let Some(ref tick) = req.tick {
        ServerTime(tick.quote.ts)
    } else {
        ServerTime(chrono::Utc::now())
    };

    let latest_tick = req.tick.clone();

    let inputs = if let Some(ref tick) = latest_tick {
        EvaluateInputs::for_tick(&positions, &trades, tick).with_equity_source(equity_source)
    } else {
        EvaluateInputs {
            open_positions: &positions,
            today_trades: &trades,
            equity_source,
            ..Default::default()
        }
    };

    let verdict = pure::evaluate(
        &acc,
        &pack,
        &registry,
        RuleContextKind::OnTick,
        server_time,
        inputs,
    )?;

    let (new_state, _events) = propfirm::engine::pipeline::apply_decision(
        propfirm::engine::state::AccountState::new(acc.clone()),
        &verdict.decision,
        server_time.0,
        "repl",
    )?;
    acc = new_state.account;

    // Output: JSON or pretty table depending on json_mode flag.
    if json_mode {
        let reports_json: Vec<serde_json::Value> = verdict
            .reports
            .iter()
            .map(|r| {
                serde_json::json!({
                    "rule_name": r.rule_name,
                    "verdict": format!("{:?}", r.verdict),
                    "priority": r.priority,
                })
            })
            .collect();
        let json_output = serde_json::json!({
            "decision_kind": format!("{:?}", verdict.decision.kind),
            "winning_priority": verdict.decision.winning_priority,
            "input_hash": verdict.input_hash,
            "pack_id": verdict.pack_id,
            "pack_version": verdict.pack_version,
            "reports": reports_json,
            "account_state": {
                "status": format!("{:?}", acc.status),
                "balance": acc.balance.to_string(),
                "equity": acc.equity.to_string(),
                "peak_balance": acc.peak_balance.to_string(),
                "daily_drawdown": acc.daily_drawdown().to_string(),
            },
        });
        Ok(serde_json::to_string_pretty(&json_output)?)
    } else {
        // Pretty-print the verdict.
        let mut out = String::new();
        out.push_str("┌─ verdict ─────────────────────────────────────────────┐\n");
        out.push_str(&format!(
            "│ decision_kind:     {:<30}            │\n",
            format!("{:?}", verdict.decision.kind)
        ));
        out.push_str(&format!(
            "│ winning_priority:  {:<30}            │\n",
            verdict.decision.winning_priority
        ));
        out.push_str(&format!("│ input_hash:        {}  │\n", verdict.input_hash));
        out.push_str(&format!(
            "│ pack_id:           {:<30}            │\n",
            verdict.pack_id
        ));
        out.push_str(&format!(
            "│ pack_version:      {:<30}            │\n",
            verdict.pack_version
        ));
        out.push_str("├─ per-rule verdicts ─────────────────────────────────┤\n");
        for report in &verdict.reports {
            let verdict_str = format!("{:?}", report.verdict);
            out.push_str(&format!(
                "│ {:<25} → {:<20} (prio={:<4}) │\n",
                report.rule_name, verdict_str, report.priority
            ));
        }
        out.push_str("├─ account state (post-eval) ───────────────────────┤\n");
        out.push_str(&format!(
            "│ status:        {:?}                                 │\n",
            acc.status
        ));
        out.push_str(&format!(
            "│ balance:       {:<30}                │\n",
            acc.balance
        ));
        out.push_str(&format!(
            "│ equity:        {:<30}                │\n",
            acc.equity
        ));
        out.push_str(&format!(
            "│ peak_balance:  {:<30}                │\n",
            acc.peak_balance
        ));
        out.push_str(&format!(
            "│ daily_drawdown: {:<29}                │\n",
            acc.daily_drawdown()
        ));
        out.push_str("└────────────────────────────────────────────────────┘");
        Ok(out)
    }
}
