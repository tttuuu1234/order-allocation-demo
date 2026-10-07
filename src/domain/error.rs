//! ドメインのエラー。
//!
//! 外部クレート(thiserror など)を使わず自前で書いているのは、
//! Rust のエラーが「ただの enum + いくつかのトレイト実装」であることを
//! そのまま見えるようにするため。
//!
//! 在庫不足はここに無い。不足は「入荷待ちにする」という正常な結果であって、
//! エラーではないから(`AllocationOutcome::Backordered`)。

use std::fmt;

use super::{OrderAction, OrderId, OrderStatus, Quantity, ShipmentNo, Sku, WarehouseId};

/// ドメインのルール違反。
///
/// Rust の enum は Swift の「associated value 付き enum」や Kotlin の sealed class と同じく、
/// バリアントごとに異なるデータを持てる。
/// `match` で全バリアントを網羅しないとコンパイルエラーになるので、
/// エラーを追加したときに HTTP 変換の書き忘れが起きない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    // ---- 入力の不正(利用者が直せる)----
    EmptySku,
    EmptyWarehouseId,
    EmptyTrackingNumber,
    /// 0 個の注文・入荷・棚卸調整
    ZeroQuantity {
        sku: Sku,
    },
    /// 明細が 1 つも無い注文
    EmptyOrderLines,
    /// 数量の上限(u32)を超えた
    QuantityOverflow {
        sku: Sku,
    },

    // ---- 業務ルール上できない操作 ----
    /// 現在の状態からは許されない操作
    InvalidTransition {
        order_id: OrderId,
        from: OrderStatus,
        action: OrderAction,
    },
    /// 出荷済み(または取り消し済み)の出荷をもう一度出そうとした
    ShipmentAlreadyShipped {
        order_id: OrderId,
        shipment_no: ShipmentNo,
    },
    /// 注文に、指定された番号の出荷が無い
    ShipmentNotFound {
        order_id: OrderId,
        shipment_no: ShipmentNo,
    },
    /// 棚卸調整で、実在庫が引当済みの数(またはマイナス)を下回る
    AdjustmentBelowReserved {
        warehouse: WarehouseId,
        sku: Sku,
        on_hand: Quantity,
        reserved: Quantity,
    },

    // ---- 内部不整合(バグでしか起きない)----
    // 業務ルールどおりに操作していれば起きないはず。
    // 起きたらバグなので、利用者の入力エラーとは区別して扱う(HTTP では 500)。
    /// 引当可能数を超えて引き当てようとした(戦略の計算違い)
    ReservedOverflow {
        warehouse: WarehouseId,
        sku: Sku,
    },
    /// 引当済み数を超えて解放・出荷しようとした
    ReservedUnderflow {
        warehouse: WarehouseId,
        sku: Sku,
    },
    /// 処理に必要な在庫レコードが渡されなかった
    StockMissing {
        warehouse: WarehouseId,
        sku: Sku,
    },
    /// 引当計画の数量が注文明細と一致しない
    PlanMismatch {
        order_id: OrderId,
    },
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `match` は Swift の `switch` に近い。全パターン網羅が必須。
        match self {
            DomainError::EmptySku => write!(f, "SKU must not be empty"),
            DomainError::EmptyWarehouseId => write!(f, "warehouse id must not be empty"),
            DomainError::EmptyTrackingNumber => write!(f, "tracking number must not be empty"),
            DomainError::ZeroQuantity { sku } => {
                write!(f, "quantity for {sku} must not be 0")
            }
            DomainError::EmptyOrderLines => write!(f, "order must have at least one line"),
            DomainError::QuantityOverflow { sku } => {
                write!(f, "quantity for {sku} is too large")
            }
            DomainError::InvalidTransition {
                order_id,
                from,
                action,
            } => write!(f, "order {order_id} cannot {action} from {from}"),
            DomainError::ShipmentAlreadyShipped {
                order_id,
                shipment_no,
            } => write!(
                f,
                "shipment {shipment_no} of order {order_id} is not awaiting shipment"
            ),
            DomainError::ShipmentNotFound {
                order_id,
                shipment_no,
            } => write!(f, "order {order_id} has no shipment {shipment_no}"),
            DomainError::AdjustmentBelowReserved {
                warehouse,
                sku,
                on_hand,
                reserved,
            } => write!(
                f,
                "cannot adjust {sku} at {warehouse}: on_hand {on_hand} would fall below reserved {reserved}"
            ),
            DomainError::ReservedOverflow { warehouse, sku } => {
                write!(
                    f,
                    "cannot reserve more than available for {sku} at {warehouse}"
                )
            }
            DomainError::ReservedUnderflow { warehouse, sku } => {
                write!(
                    f,
                    "reserved quantity of {sku} at {warehouse} would become negative"
                )
            }
            DomainError::StockMissing { warehouse, sku } => {
                write!(f, "stock record for {sku} at {warehouse} is missing")
            }
            DomainError::PlanMismatch { order_id } => {
                write!(
                    f,
                    "allocation plan does not match lines of order {order_id}"
                )
            }
        }
    }
}

// 標準のエラートレイトを実装しておくと、`Box<dyn std::error::Error>` などの
// 汎用的なエラー処理に乗せられる。中身は Display と Debug があれば空でよい。
impl std::error::Error for DomainError {}
