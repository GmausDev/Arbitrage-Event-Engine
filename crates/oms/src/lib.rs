// crates/oms/src/lib.rs
//
// Order Management System — tracks the full lifecycle of every order placed
// on any exchange, converts exchange fills into `Event::Execution` events,
// and provides a unified view of open orders + balances across exchanges.
//
// The OMS sits between the risk engine and the exchange connectors:
//
//   risk_engine → Event::ApprovedTrade
//       → OMS.submit_order(exchange, OrderRequest)
//           → ExchangeConnector.place_order()
//           → OMS tracks: Pending → Open → PartiallyFilled → Filled
//           → Publishes Event::Execution on the bus
//
// The OMS is also responsible for:
//   - Pre-flight checks (balance sufficiency, min order size)
//   - Order timeout / staleness cancellation
//   - Reconciliation with exchange state

pub mod manager;
pub mod state;

pub use manager::OrderManager;
pub use state::{ManagedOrder, OrderManagerState};
