//! ドメインのエラー。
//!
//! 外部クレート(thiserror など)を使わず自前で書いているのは、
//! Rust のエラーが「ただの enum + いくつかのトレイト実装」であることを
//! そのまま見えるようにするため。

use std::fmt;

use super::{OrderAction, OrderId, OrderStatus, Quantity, Sku};

/// 1 明細ぶんの在庫不足。
///
/// 不足時は「どれか 1 つ」ではなく「足りない明細すべて」を返す。
/// 利用者が数量を直して再注文するとき、1 つずつエラーを潰すやり取りを避けるため。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortage {
    pub sku: Sku,
    pub requested: Quantity,
    pub available: Quantity,
}

impl Shortage {
    /// 不足数。表示や判断に使えるよう計算で求める(保存すると不整合の元になる)。
    pub fn missing(&self) -> Quantity {
        self.requested.saturating_sub(self.available)
    }
}

/// ドメインのルール違反。
///
/// Rust の enum は Swift の「associated value 付き enum」や Kotlin の sealed class と同じく、
/// バリアントごとに異なるデータを持てる。
/// `match` で全バリアントを網羅しないとコンパイルエラーになるので、
/// エラーを追加したときに HTTP 変換の書き忘れが起きない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    /// SKU が空。
    EmptySku,
    /// 0 個の注文・入荷。
    ZeroQuantity { sku: Sku },
    /// 明細が 1 つも無い注文。
    EmptyOrderLines,
    /// 数量の上限(u32)を超えた。
    QuantityOverflow { sku: Sku },
    /// 在庫不足。全明細の不足をまとめて持つ。
    InsufficientStock { shortages: Vec<Shortage> },
    /// 現在の状態からは許されない操作。
    InvalidTransition {
        order_id: OrderId,
        from: OrderStatus,
        action: OrderAction,
    },
    /// 引当済み数を超えて解放・出荷しようとした。
    ///
    /// 業務ルールどおりに操作していれば起きないはずの「内部不整合」。
    /// 起きたらバグなので、利用者の入力エラーとは区別して扱う。
    ReservedUnderflow { sku: Sku },
    /// 処理に必要な在庫レコードが渡されなかった。
    ///
    /// これも呼び出し側(サービス層)のバグでしか起きない。
    StockMissing { sku: Sku },
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `match` は Swift の `switch` に近い。全パターン網羅が必須。
        match self {
            DomainError::EmptySku => write!(f, "SKU must not be empty"),
            DomainError::ZeroQuantity { sku } => {
                write!(f, "quantity for {sku} must be greater than 0")
            }
            DomainError::EmptyOrderLines => write!(f, "order must have at least one line"),
            DomainError::QuantityOverflow { sku } => {
                write!(f, "quantity for {sku} is too large")
            }
            DomainError::InsufficientStock { shortages } => {
                // 不足明細を「APPLE (requested 5, available 3)」の形で並べる。
                let details: Vec<String> = shortages
                    .iter()
                    .map(|s| {
                        format!(
                            "{} (requested {}, available {})",
                            s.sku, s.requested, s.available
                        )
                    })
                    .collect();
                write!(f, "insufficient stock: {}", details.join(", "))
            }
            DomainError::InvalidTransition {
                order_id,
                from,
                action,
            } => write!(f, "order {order_id} cannot {action} from {from}"),
            DomainError::ReservedUnderflow { sku } => {
                write!(f, "reserved quantity of {sku} would become negative")
            }
            DomainError::StockMissing { sku } => write!(f, "stock record for {sku} is missing"),
        }
    }
}

// 標準のエラートレイトを実装しておくと、`Box<dyn std::error::Error>` などの
// 汎用的なエラー処理に乗せられる。中身は Display と Debug があれば空でよい。
impl std::error::Error for DomainError {}
