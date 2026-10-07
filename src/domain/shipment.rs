//! 出荷。1 件 = 「1 つの倉庫から 1 回で出す荷物」。
//!
//! 注文(Order)と出荷を分けるのは、実務では 1 つの注文が複数の荷物に分かれるから。
//! 例えば APPLE は東京倉庫、ORANGE は大阪倉庫から出すなら、荷物も送り状番号も 2 つになる。
//!
//! 出荷は Order 集約の「子」として Order の中に持つ。理由は次の 2 つ。
//! - 「全出荷が終わったら注文も出荷済み」という整合性を、Order 1 つの中で守れる
//! - 1 回の保存で注文と出荷を必ず一緒に書ける(片方だけ更新される心配が無い)
//!
//! 出荷を倉庫側の別システム(WMS)が持つ実務構成なら、独立した集約にして
//! イベントで注文側に通知する形になる(README のトレードオフ参照)。

use std::fmt;

use super::{DomainError, Quantity, Sku, WarehouseId};

/// 注文の中での出荷番号(1, 2, ...)。
///
/// 全体で一意な ID にせず「注文 ID + 出荷番号」で識別するのは、
/// 出荷が Order 集約の子で、Order を通してしか扱わないため。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShipmentNo(u32);

impl ShipmentNo {
    pub fn new(value: u32) -> Self {
        ShipmentNo(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ShipmentNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 送り状番号(追跡番号)。運送会社が発行する。空は認めない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackingNumber(String);

impl TrackingNumber {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(DomainError::EmptyTrackingNumber);
        }
        Ok(TrackingNumber(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 出荷の状態。
///
/// `Shipped` だけがデータ(送り状番号)を持つ。
/// 「出荷済みなら必ず送り状番号がある」「出荷前には無い」を、
/// `Option<TrackingNumber>` を別に持つより確実に型で表現できる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShipmentStatus {
    /// 引当済みで、倉庫での作業(ピッキング・梱包)と出荷を待っている
    AwaitingShipment,
    /// 出荷済み
    Shipped { tracking_number: TrackingNumber },
    /// 注文のキャンセルにより取り消し
    Cancelled,
}

/// 出荷の明細。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShipmentLine {
    pub sku: Sku,
    pub quantity: Quantity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shipment {
    no: ShipmentNo,
    warehouse: WarehouseId,
    lines: Vec<ShipmentLine>,
    status: ShipmentStatus,
}

impl Shipment {
    /// 出荷待ちの出荷を作る。作るのは Order(引当時)だけ。
    ///
    /// `pub(super)` は「親モジュール(domain)の中からだけ呼べる」という公開範囲。
    /// API やサービスから勝手に出荷を作れないようにしている。
    pub(super) fn new(no: ShipmentNo, warehouse: WarehouseId, lines: Vec<ShipmentLine>) -> Self {
        Shipment {
            no,
            warehouse,
            lines,
            status: ShipmentStatus::AwaitingShipment,
        }
    }

    pub fn no(&self) -> ShipmentNo {
        self.no
    }

    pub fn warehouse(&self) -> &WarehouseId {
        &self.warehouse
    }

    pub fn lines(&self) -> &[ShipmentLine] {
        &self.lines
    }

    pub fn status(&self) -> &ShipmentStatus {
        &self.status
    }

    /// まだ在庫を押さえている(=引当中)か。
    pub fn is_awaiting(&self) -> bool {
        self.status == ShipmentStatus::AwaitingShipment
    }

    pub(super) fn mark_shipped(&mut self, tracking_number: TrackingNumber) {
        self.status = ShipmentStatus::Shipped { tracking_number };
    }

    pub(super) fn mark_cancelled(&mut self) {
        self.status = ShipmentStatus::Cancelled;
    }
}
