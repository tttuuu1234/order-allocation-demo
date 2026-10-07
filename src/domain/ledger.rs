//! 在庫の受払(うけはらい)台帳。
//!
//! 実在庫(on_hand)が変わるたびに「いつ・どこで・何が・いくつ・なぜ」を 1 行ずつ記録する。
//! 在庫数を上書きするだけだと、数が合わなくなったときに原因を追えないため。
//! 帳簿でいう「仕訳」にあたり、記録は追記のみで、後から書き換えない。
//!
//! 台帳に載るのは on_hand の増減(物の出入り)だけ。
//! 引当(reserved)は「まだ倉庫から物が動いていない予約」なので受払ではなく、
//! 注文側の出荷(Shipment)が持っている。

use super::{OrderId, Quantity, ShipmentNo, Sku, WarehouseId};

/// 棚卸調整の理由。
///
/// 自由文字列にしないのは、理由ごとに集計(「今月の破損は何個か」)できるようにするため。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjustmentReason {
    /// 破損・汚損
    Damaged,
    /// 紛失(実数が帳簿より少ない)
    Lost,
    /// 発見(実数が帳簿より多い)
    Found,
    /// 棚卸での数え直し
    CountCorrection,
}

/// 在庫が動いた理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovementReason {
    /// 入荷
    Receipt,
    /// 出荷。どの注文のどの出荷かを残して、出荷実績と突き合わせられるようにする。
    Shipment {
        order_id: OrderId,
        shipment_no: ShipmentNo,
    },
    /// 棚卸調整
    Adjustment(AdjustmentReason),
}

/// 1 回の在庫の動き。Stock のメソッドが作る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockMovement {
    pub warehouse: WarehouseId,
    pub sku: Sku,
    /// 増減。入荷はプラス、出荷はマイナス。
    ///
    /// Quantity(u32)はマイナスを表せないので、ここだけ符号付きの i64 を使う。
    /// u32 の全範囲を正負どちらでも表せるよう、i32 ではなく i64 にしている。
    pub delta: i64,
    /// 動いた後の実在庫。紙の台帳の「残高」欄にあたり、行ごとに検算できる。
    pub balance_after: Quantity,
    pub reason: MovementReason,
}

/// 台帳に記録済みの 1 行。保存先が連番(seq)を振る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub seq: u64,
    pub movement: StockMovement,
}
