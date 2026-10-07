//! ドメイン層。業務ルールだけを表現する純粋なロジック。
//!
//! serde や axum には依存させない。HTTP や JSON はドメインの関心ではなく、
//! それらに引きずられると「業務ルールのテストに Web サーバが必要」になってしまうため。

mod allocation;
mod error;
mod ledger;
mod order;
mod shipment;
mod stock;
mod strategy;
mod types;

// `pub use` で再公開しておくと、利用側は `domain::Order` のように
// 内部のファイル構成を知らずに書ける。ファイルを分割し直しても利用側が壊れない。
pub use allocation::{AllocationOutcome, allocate, cancel, reallocate_backorders, ship};
pub use error::DomainError;
pub use ledger::{AdjustmentReason, LedgerEntry, MovementReason, StockMovement};
pub use order::{Order, OrderAction, OrderLine, OrderStatus};
pub use shipment::{Shipment, ShipmentLine, ShipmentNo, ShipmentStatus, TrackingNumber};
pub use stock::Stock;
pub use strategy::{
    AllocationStrategy, GreedyByPriority, PlannedAllocation, PreferSingleWarehouse, Shortage,
};
pub use types::{OrderId, Quantity, Sku, WarehouseId};
