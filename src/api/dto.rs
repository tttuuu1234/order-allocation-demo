//! リクエスト/レスポンスの DTO。
//!
//! DTO をドメイン型と分けているのは、JSON の形(API の契約)とドメインの内部表現を
//! 独立に変えられるようにするため。ドメイン型に serde を付けない理由でもある。

use serde::{Deserialize, Serialize};

use crate::domain::{
    AdjustmentReason, LedgerEntry, MovementReason, Order, Shipment, ShipmentStatus, Shortage, Stock,
};
use crate::service::InventoryChange;

// ---------------- リクエスト ----------------

/// `#[derive(Deserialize)]` で、JSON からこの構造体への変換コードが自動生成される。
#[derive(Debug, Deserialize)]
pub struct ReceiptRequest {
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
pub struct AdjustmentRequest {
    /// 増減。減らすときはマイナス。
    pub delta: i64,
    pub reason: AdjustmentReasonDto,
}

/// 棚卸調整の理由。JSON では `"Damaged"` のような文字列で受け取る。
///
/// serde は enum のバリアント名と文字列を自動で対応させる。
/// 知らない文字列が来たら、ハンドラに届く前に 422 になる。
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum AdjustmentReasonDto {
    Damaged,
    Lost,
    Found,
    CountCorrection,
}

impl From<AdjustmentReasonDto> for AdjustmentReason {
    fn from(dto: AdjustmentReasonDto) -> Self {
        match dto {
            AdjustmentReasonDto::Damaged => AdjustmentReason::Damaged,
            AdjustmentReasonDto::Lost => AdjustmentReason::Lost,
            AdjustmentReasonDto::Found => AdjustmentReason::Found,
            AdjustmentReasonDto::CountCorrection => AdjustmentReason::CountCorrection,
        }
    }
}

impl From<AdjustmentReason> for AdjustmentReasonDto {
    fn from(reason: AdjustmentReason) -> Self {
        match reason {
            AdjustmentReason::Damaged => AdjustmentReasonDto::Damaged,
            AdjustmentReason::Lost => AdjustmentReasonDto::Lost,
            AdjustmentReason::Found => AdjustmentReasonDto::Found,
            AdjustmentReason::CountCorrection => AdjustmentReasonDto::CountCorrection,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub lines: Vec<LineDto>,
}

/// 注文明細・出荷明細の共通の形。
#[derive(Debug, Deserialize, Serialize)]
pub struct LineDto {
    pub sku: String,
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
pub struct ShipRequest {
    pub tracking_number: String,
}

// ---------------- レスポンス: 在庫 ----------------

#[derive(Debug, Serialize)]
pub struct StockResponse {
    pub warehouse: String,
    pub sku: String,
    pub on_hand: u32,
    pub reserved: u32,
    pub available: u32,
}

// `From` を実装しておくと `StockResponse::from(&stock)` で変換できる。
// 引数を `&Stock`(参照)にしているのは、変換のために Stock の所有権を奪う必要が無いから。
impl From<&Stock> for StockResponse {
    fn from(stock: &Stock) -> Self {
        StockResponse {
            warehouse: stock.warehouse().to_string(),
            sku: stock.sku().to_string(),
            on_hand: stock.on_hand().value(),
            reserved: stock.reserved().value(),
            available: stock.available().value(),
        }
    }
}

/// 1 SKU の、全倉庫合計と倉庫別の内訳。
#[derive(Debug, Serialize)]
pub struct StockSummaryResponse {
    pub sku: String,
    pub on_hand: u32,
    pub reserved: u32,
    pub available: u32,
    pub warehouses: Vec<StockResponse>,
}

impl StockSummaryResponse {
    /// `stocks` は同じ SKU の在庫で、1 件以上ある前提(サービスが保証している)。
    pub fn new(stocks: &[Stock]) -> Self {
        let sum = |f: fn(&Stock) -> u32| stocks.iter().map(f).sum();
        StockSummaryResponse {
            sku: stocks
                .first()
                .map(|s| s.sku().to_string())
                .unwrap_or_default(),
            on_hand: sum(|s| s.on_hand().value()),
            reserved: sum(|s| s.reserved().value()),
            available: sum(|s| s.available().value()),
            warehouses: stocks.iter().map(StockResponse::from).collect(),
        }
    }
}

/// 受払台帳の 1 行。
///
/// `skip_serializing_if = "Option::is_none"` で、None の項目はキーごと出力しない。
/// 理由の種類によって付く情報が違うので、関係ない項目を null で並べないようにしている。
#[derive(Debug, Serialize)]
pub struct LedgerEntryResponse {
    pub seq: u64,
    pub warehouse: String,
    pub sku: String,
    pub delta: i64,
    pub balance_after: u32,
    /// "Receipt" / "Shipment" / "Adjustment"
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shipment_no: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adjustment_reason: Option<AdjustmentReasonDto>,
}

impl From<&LedgerEntry> for LedgerEntryResponse {
    fn from(entry: &LedgerEntry) -> Self {
        let m = &entry.movement;
        let mut response = LedgerEntryResponse {
            seq: entry.seq,
            warehouse: m.warehouse.to_string(),
            sku: m.sku.to_string(),
            delta: m.delta,
            balance_after: m.balance_after.value(),
            reason: "Receipt",
            order_id: None,
            shipment_no: None,
            adjustment_reason: None,
        };
        match &m.reason {
            MovementReason::Receipt => {}
            MovementReason::Shipment {
                order_id,
                shipment_no,
            } => {
                response.reason = "Shipment";
                response.order_id = Some(order_id.value());
                response.shipment_no = Some(shipment_no.value());
            }
            MovementReason::Adjustment(reason) => {
                response.reason = "Adjustment";
                response.adjustment_reason = Some((*reason).into());
            }
        }
        response
    }
}

/// 入荷・棚卸調整のレスポンス。
#[derive(Debug, Serialize)]
pub struct InventoryChangeResponse {
    pub stock: StockResponse,
    pub ledger_entry: LedgerEntryResponse,
    /// この変更で入荷待ちから引当済みになった注文
    pub reallocated_orders: Vec<u64>,
}

impl From<&InventoryChange> for InventoryChangeResponse {
    fn from(change: &InventoryChange) -> Self {
        InventoryChangeResponse {
            stock: StockResponse::from(&change.stock),
            ledger_entry: LedgerEntryResponse::from(&change.entry),
            reallocated_orders: change.reallocated.iter().map(|id| id.value()).collect(),
        }
    }
}

// ---------------- レスポンス: 受注 ----------------

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub id: u64,
    /// ドメインの状態名をそのまま出す("Pending", "Backordered", ...)
    pub status: String,
    pub lines: Vec<LineDto>,
    pub shipments: Vec<ShipmentResponse>,
}

impl From<&Order> for OrderResponse {
    fn from(order: &Order) -> Self {
        OrderResponse {
            id: order.id().value(),
            status: order.status().to_string(),
            lines: order
                .lines()
                .iter()
                .map(|line| LineDto {
                    sku: line.sku.to_string(),
                    quantity: line.quantity.value(),
                })
                .collect(),
            shipments: order
                .shipments()
                .iter()
                .map(ShipmentResponse::from)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ShipmentResponse {
    pub no: u32,
    pub warehouse: String,
    /// "AwaitingShipment" / "Shipped" / "Cancelled"
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking_number: Option<String>,
    pub lines: Vec<LineDto>,
}

impl From<&Shipment> for ShipmentResponse {
    fn from(shipment: &Shipment) -> Self {
        // 1 つの match で、状態名と送り状番号を同時に取り出す。
        let (status, tracking_number) = match shipment.status() {
            ShipmentStatus::AwaitingShipment => ("AwaitingShipment", None),
            ShipmentStatus::Shipped { tracking_number } => {
                ("Shipped", Some(tracking_number.as_str().to_string()))
            }
            ShipmentStatus::Cancelled => ("Cancelled", None),
        };
        ShipmentResponse {
            no: shipment.no().value(),
            warehouse: shipment.warehouse().to_string(),
            status,
            tracking_number,
            lines: shipment
                .lines()
                .iter()
                .map(|line| LineDto {
                    sku: line.sku.to_string(),
                    quantity: line.quantity.value(),
                })
                .collect(),
        }
    }
}

/// 引当のレスポンス。入荷待ちになったときだけ `shortages` が付く。
///
/// 在庫不足を 409 エラーにせず 200 で返すのは、入荷待ちという状態への遷移は成功しているから。
#[derive(Debug, Serialize)]
pub struct AllocateResponse {
    pub order: OrderResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortages: Option<Vec<ShortageResponse>>,
}

#[derive(Debug, Serialize)]
pub struct ShortageResponse {
    pub sku: String,
    pub requested: u32,
    pub available: u32,
    pub missing: u32,
}

impl From<&Shortage> for ShortageResponse {
    fn from(s: &Shortage) -> Self {
        ShortageResponse {
            sku: s.sku.to_string(),
            requested: s.requested.value(),
            available: s.available.value(),
            missing: s.missing().value(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    pub order: OrderResponse,
    /// 解放された在庫で、入荷待ちから引当済みになった注文
    pub reallocated_orders: Vec<u64>,
}
