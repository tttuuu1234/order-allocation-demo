//! サービス層のエラー。

use std::fmt;

use crate::domain::{DomainError, OrderId, Sku, WarehouseId};
use crate::repository::RepositoryError;

/// サービス層のエラー。ドメインのエラーに、「見つからない」と永続化の失敗を足したもの。
///
/// 「見つからない」をドメインではなくここに置いたのは、
/// 「ID で探して無かった」というのは保存先の事情であって業務ルールではないため。
/// (出荷番号の未存在だけは、Order 集約の中の話なのでドメインにある)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Domain(DomainError),
    /// `warehouse` が None なら「どの倉庫にも無い」
    StockNotFound {
        sku: Sku,
        warehouse: Option<WarehouseId>,
    },
    OrderNotFound(OrderId),
    /// 注文に、どの倉庫にも在庫レコードの無い SKU が含まれていた。まとめて返す。
    UnknownSkus(Vec<Sku>),
    Repository(RepositoryError),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Domain(e) => write!(f, "{e}"),
            // `Some(w)` / `None` のように、Option の中身ごとに分けて書ける。
            ServiceError::StockNotFound {
                sku,
                warehouse: Some(w),
            } => write!(f, "stock {sku} not found at {w}"),
            ServiceError::StockNotFound {
                sku,
                warehouse: None,
            } => write!(f, "stock {sku} not found"),
            ServiceError::OrderNotFound(id) => write!(f, "order {id} not found"),
            ServiceError::UnknownSkus(skus) => {
                let names: Vec<&str> = skus.iter().map(Sku::as_str).collect();
                write!(f, "unknown SKU: {}", names.join(", "))
            }
            ServiceError::Repository(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ServiceError {}

// `impl From<A> for B` を書くと、`?` 演算子が A のエラーを自動で B に変換してくれる。
// これにより、戻り値が `Result<_, ServiceError>` の関数の中で、
// `Result<_, DomainError>` を返す関数に `?` をそのまま付けられる。
impl From<DomainError> for ServiceError {
    fn from(e: DomainError) -> Self {
        ServiceError::Domain(e)
    }
}

impl From<RepositoryError> for ServiceError {
    fn from(e: RepositoryError) -> Self {
        ServiceError::Repository(e)
    }
}
