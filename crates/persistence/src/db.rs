// crates/persistence/src/db.rs
//
// SQLite database wrapper.  Creates schema on first run, provides typed
// insert/query methods for all persisted record types.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tracing::info;

use crate::models::{AuditEntry, EquitySnapshot, TradeRecord};

/// SQLite-backed persistence store.
///
/// The inner `Connection` is wrapped in a `Mutex` so that `Database` is
/// `Send + Sync` and can be shared via `Arc<Database>` across async tasks.
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open (or create) a database at the given path.
    ///
    /// Pass `":memory:"` for an ephemeral in-memory database (tests).
    pub fn open(path: &str) -> Result<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            // Ensure parent directory exists.
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            Connection::open(path)?
        };

        // WAL mode for concurrent reads during writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let db = Self { conn: Mutex::new(conn) };
        db.create_schema()?;
        Ok(db)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("persistence: database mutex poisoned")
    }

    fn create_schema(&self) -> Result<()> {
        self.conn().execute_batch(
            "
            CREATE TABLE IF NOT EXISTS trades (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                exchange            TEXT NOT NULL,
                order_id            TEXT NOT NULL,
                market_id           TEXT NOT NULL,
                side                TEXT NOT NULL,
                signal_source       TEXT NOT NULL DEFAULT '',

                requested_size_usd  REAL NOT NULL,
                filled_size_usd     REAL NOT NULL,
                fill_ratio          REAL NOT NULL,

                market_price        REAL NOT NULL,
                posterior_prob      REAL NOT NULL,
                avg_fill_price      REAL NOT NULL,

                gross_edge          REAL NOT NULL,
                fee_cost            REAL NOT NULL,
                spread_cost         REAL NOT NULL,
                slippage_cost       REAL NOT NULL,
                decay_cost          REAL NOT NULL,
                total_cost          REAL NOT NULL,
                net_edge            REAL NOT NULL,
                realized_slippage   REAL NOT NULL,
                expected_profit_usd REAL NOT NULL,

                status              TEXT NOT NULL,
                signal_timestamp    TEXT NOT NULL,
                submitted_at        TEXT NOT NULL,
                filled_at           TEXT NOT NULL,
                latency_ms          INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_trades_market     ON trades(market_id);
            CREATE INDEX IF NOT EXISTS idx_trades_exchange    ON trades(exchange);
            CREATE INDEX IF NOT EXISTS idx_trades_source      ON trades(signal_source);
            CREATE INDEX IF NOT EXISTS idx_trades_filled_at   ON trades(filled_at);

            CREATE TABLE IF NOT EXISTS equity_snapshots (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                equity_usd      REAL NOT NULL,
                cash_usd        REAL NOT NULL,
                deployed_usd    REAL NOT NULL,
                open_positions  INTEGER NOT NULL,
                realized_pnl    REAL NOT NULL,
                unrealized_pnl  REAL NOT NULL,
                peak_equity     REAL NOT NULL,
                drawdown        REAL NOT NULL,
                timestamp       TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_equity_ts ON equity_snapshots(timestamp);

            CREATE TABLE IF NOT EXISTS audit_log (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                category     TEXT NOT NULL,
                summary      TEXT NOT NULL,
                details_json TEXT NOT NULL DEFAULT '{}',
                timestamp    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_audit_cat ON audit_log(category);
            CREATE INDEX IF NOT EXISTS idx_audit_ts  ON audit_log(timestamp);
            ",
        )?;

        info!("persistence: schema initialized");
        Ok(())
    }

    // ── Trade records ────────────────────────────────────────────────────────

    pub fn insert_trade(&self, t: &TradeRecord) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO trades (
                exchange, order_id, market_id, side, signal_source,
                requested_size_usd, filled_size_usd, fill_ratio,
                market_price, posterior_prob, avg_fill_price,
                gross_edge, fee_cost, spread_cost, slippage_cost, decay_cost,
                total_cost, net_edge, realized_slippage, expected_profit_usd,
                status, signal_timestamp, submitted_at, filled_at, latency_ms
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8,
                ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, ?24, ?25
            )",
            params![
                t.exchange, t.order_id, t.market_id, t.side, t.signal_source,
                t.requested_size_usd, t.filled_size_usd, t.fill_ratio,
                t.market_price, t.posterior_prob, t.avg_fill_price,
                t.gross_edge, t.fee_cost, t.spread_cost, t.slippage_cost, t.decay_cost,
                t.total_cost, t.net_edge, t.realized_slippage, t.expected_profit_usd,
                t.status, t.signal_timestamp.to_rfc3339(), t.submitted_at.to_rfc3339(),
                t.filled_at.to_rfc3339(), t.latency_ms,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_recent_trades(&self, limit: usize) -> Result<Vec<TradeRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM trades ORDER BY filled_at DESC LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(TradeRecord {
                id:                  Some(row.get(0)?),
                exchange:            row.get(1)?,
                order_id:            row.get(2)?,
                market_id:           row.get(3)?,
                side:                row.get(4)?,
                signal_source:       row.get(5)?,
                requested_size_usd:  row.get(6)?,
                filled_size_usd:     row.get(7)?,
                fill_ratio:          row.get(8)?,
                market_price:        row.get(9)?,
                posterior_prob:       row.get(10)?,
                avg_fill_price:      row.get(11)?,
                gross_edge:          row.get(12)?,
                fee_cost:            row.get(13)?,
                spread_cost:         row.get(14)?,
                slippage_cost:       row.get(15)?,
                decay_cost:          row.get(16)?,
                total_cost:          row.get(17)?,
                net_edge:            row.get(18)?,
                realized_slippage:   row.get(19)?,
                expected_profit_usd: row.get(20)?,
                status:              row.get(21)?,
                signal_timestamp:    parse_dt(row.get::<_, String>(22)?),
                submitted_at:        parse_dt(row.get::<_, String>(23)?),
                filled_at:           parse_dt(row.get::<_, String>(24)?),
                latency_ms:          row.get(25)?,
            })
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Aggregate P&L by signal source.
    pub fn pnl_by_source(&self) -> Result<Vec<(String, f64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT signal_source,
                    SUM(net_edge * filled_size_usd) as total_pnl,
                    COUNT(*) as trade_count
             FROM trades
             WHERE status = 'filled'
             GROUP BY signal_source
             ORDER BY total_pnl DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, i64>(2)?))
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Aggregate cost breakdown across all trades.
    pub fn cost_summary(&self) -> Result<CostSummary> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT COUNT(*),
                    SUM(fee_cost * filled_size_usd),
                    SUM(spread_cost * filled_size_usd),
                    SUM(slippage_cost * filled_size_usd),
                    SUM(decay_cost * filled_size_usd),
                    SUM(total_cost * filled_size_usd),
                    AVG(realized_slippage),
                    SUM(filled_size_usd)
             FROM trades
             WHERE status = 'filled'",
        )?;

        stmt.query_row([], |row| {
            Ok(CostSummary {
                trade_count:        row.get(0)?,
                total_fees_usd:     row.get::<_, f64>(1).unwrap_or(0.0),
                total_spread_usd:   row.get::<_, f64>(2).unwrap_or(0.0),
                total_slippage_usd: row.get::<_, f64>(3).unwrap_or(0.0),
                total_decay_usd:    row.get::<_, f64>(4).unwrap_or(0.0),
                total_cost_usd:     row.get::<_, f64>(5).unwrap_or(0.0),
                avg_slippage:       row.get::<_, f64>(6).unwrap_or(0.0),
                total_volume_usd:   row.get::<_, f64>(7).unwrap_or(0.0),
            })
        }).map_err(Into::into)
    }

    // ── Equity snapshots ─────────────────────────────────────────────────────

    pub fn insert_equity_snapshot(&self, s: &EquitySnapshot) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO equity_snapshots (
                equity_usd, cash_usd, deployed_usd, open_positions,
                realized_pnl, unrealized_pnl, peak_equity, drawdown, timestamp
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                s.equity_usd, s.cash_usd, s.deployed_usd, s.open_positions,
                s.realized_pnl, s.unrealized_pnl, s.peak_equity, s.drawdown,
                s.timestamp.to_rfc3339(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_equity_curve(&self, limit: usize) -> Result<Vec<EquitySnapshot>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM equity_snapshots ORDER BY timestamp DESC LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(EquitySnapshot {
                id:              Some(row.get(0)?),
                equity_usd:      row.get(1)?,
                cash_usd:        row.get(2)?,
                deployed_usd:    row.get(3)?,
                open_positions:  row.get(4)?,
                realized_pnl:    row.get(5)?,
                unrealized_pnl:  row.get(6)?,
                peak_equity:     row.get(7)?,
                drawdown:        row.get(8)?,
                timestamp:       parse_dt(row.get::<_, String>(9)?),
            })
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ── Audit log ────────────────────────────────────────────────────────────

    pub fn insert_audit(&self, entry: &AuditEntry) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO audit_log (category, summary, details_json, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                entry.category, entry.summary, entry.details_json,
                entry.timestamp.to_rfc3339(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_audit_log(&self, category: Option<&str>, limit: usize) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let (sql, p): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match category {
            Some(cat) => (
                "SELECT * FROM audit_log WHERE category = ?1 ORDER BY timestamp DESC LIMIT ?2".into(),
                vec![Box::new(cat.to_string()), Box::new(limit as i64)],
            ),
            None => (
                "SELECT * FROM audit_log ORDER BY timestamp DESC LIMIT ?1".into(),
                vec![Box::new(limit as i64)],
            ),
        };

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(p.iter()), |row| {
            Ok(AuditEntry {
                id:           Some(row.get(0)?),
                category:     row.get(1)?,
                summary:      row.get(2)?,
                details_json: row.get(3)?,
                timestamp:    parse_dt(row.get::<_, String>(4)?),
            })
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Total number of trades stored.
    pub fn trade_count(&self) -> Result<i64> {
        Ok(self.conn().query_row("SELECT COUNT(*) FROM trades", [], |r| r.get(0))?)
    }
}

/// Aggregate cost summary across all filled trades.
#[derive(Debug, Clone, Default)]
pub struct CostSummary {
    pub trade_count: i64,
    pub total_fees_usd: f64,
    pub total_spread_usd: f64,
    pub total_slippage_usd: f64,
    pub total_decay_usd: f64,
    pub total_cost_usd: f64,
    pub avg_slippage: f64,
    pub total_volume_usd: f64,
}

fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open(":memory:").unwrap()
    }

    fn sample_trade() -> TradeRecord {
        TradeRecord {
            id: None,
            exchange: "polymarket".into(),
            order_id: "test-001".into(),
            market_id: "WILL_BTC_100K".into(),
            side: "buy_yes".into(),
            signal_source: "signal_agent".into(),
            requested_size_usd: 500.0,
            filled_size_usd: 500.0,
            fill_ratio: 1.0,
            market_price: 0.55,
            posterior_prob: 0.62,
            avg_fill_price: 0.56,
            gross_edge: 0.07,
            fee_cost: 0.005,
            spread_cost: 0.01,
            slippage_cost: 0.0001,
            decay_cost: 0.001,
            total_cost: 0.0161,
            net_edge: 0.0539,
            realized_slippage: 0.018,
            expected_profit_usd: 26.95,
            status: "filled".into(),
            signal_timestamp: Utc::now(),
            submitted_at: Utc::now(),
            filled_at: Utc::now(),
            latency_ms: 150,
        }
    }

    #[test]
    fn insert_and_query_trade() {
        let db = test_db();
        let trade = sample_trade();
        let id = db.insert_trade(&trade).unwrap();
        assert!(id > 0);

        let trades = db.get_recent_trades(10).unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].market_id, "WILL_BTC_100K");
        assert_eq!(trades[0].exchange, "polymarket");
    }

    #[test]
    fn pnl_by_source_aggregation() {
        let db = test_db();
        let mut t1 = sample_trade();
        t1.signal_source = "signal_agent".into();
        t1.net_edge = 0.05;
        t1.filled_size_usd = 100.0;
        db.insert_trade(&t1).unwrap();

        let mut t2 = sample_trade();
        t2.signal_source = "graph_arb_agent".into();
        t2.net_edge = 0.08;
        t2.filled_size_usd = 200.0;
        db.insert_trade(&t2).unwrap();

        let pnl = db.pnl_by_source().unwrap();
        assert_eq!(pnl.len(), 2);
        // graph_arb_agent has higher PnL (0.08 * 200 = 16) vs signal_agent (0.05 * 100 = 5)
        assert_eq!(pnl[0].0, "graph_arb_agent");
    }

    #[test]
    fn insert_and_query_equity_snapshot() {
        let db = test_db();
        let snap = EquitySnapshot {
            id: None,
            equity_usd: 10_150.0,
            cash_usd: 8_000.0,
            deployed_usd: 2_000.0,
            open_positions: 3,
            realized_pnl: 100.0,
            unrealized_pnl: 50.0,
            peak_equity: 10_200.0,
            drawdown: 0.005,
            timestamp: Utc::now(),
        };
        db.insert_equity_snapshot(&snap).unwrap();

        let curve = db.get_equity_curve(10).unwrap();
        assert_eq!(curve.len(), 1);
        assert!((curve[0].equity_usd - 10_150.0).abs() < 0.01);
    }

    #[test]
    fn insert_and_query_audit() {
        let db = test_db();
        let entry = AuditEntry {
            id: None,
            category: "trade".into(),
            summary: "Order placed on Polymarket".into(),
            details_json: r#"{"market":"BTC"}"#.into(),
            timestamp: Utc::now(),
        };
        db.insert_audit(&entry).unwrap();

        let log = db.get_audit_log(Some("trade"), 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].category, "trade");
    }

    #[test]
    fn cost_summary_aggregation() {
        let db = test_db();
        db.insert_trade(&sample_trade()).unwrap();
        let summary = db.cost_summary().unwrap();
        assert_eq!(summary.trade_count, 1);
        assert!(summary.total_volume_usd > 0.0);
    }

    #[test]
    fn trade_count() {
        let db = test_db();
        assert_eq!(db.trade_count().unwrap(), 0);
        db.insert_trade(&sample_trade()).unwrap();
        assert_eq!(db.trade_count().unwrap(), 1);
    }
}
