//! 各エンドポイントの処理。
//!
//! どのハンドラも「パスと本文を読む → ドメイン型に変換 → サービスを呼ぶ → DTO にして返す」
//! だけで、業務の判断はしない。
//!
//! 引数の `Result<Json<T>, JsonRejection>` は「本文の読み取りに失敗しても
//! ハンドラまで来させる」ための書き方。中で `?` を使うと、`impl From` により
//! ApiError に変換されて返る。
//!
//! サービスのメソッドは同期関数(ロック中に .await しない短い処理)なので、
//! async ハンドラからそのまま呼んでいる。重い処理なら spawn_blocking を検討する。

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;

use super::AppService;
use super::dto::*;
use super::error::ApiError;
use crate::domain::{
    AllocationOutcome, DomainError, OrderId, OrderLine, Quantity, ShipmentNo, Sku, TrackingNumber,
    WarehouseId,
};

// Swift の typealias と同じく、長い型に短い名前を付ける。
type AppState = State<Arc<AppService>>;
type ApiResult<T> = Result<Json<T>, ApiError>;
type CreatedResult<T> = Result<(StatusCode, Json<T>), ApiError>;

// ---------------- 在庫 ----------------

pub async fn list_stocks(State(service): AppState) -> ApiResult<Vec<StockResponse>> {
    let stocks = service.list_stocks()?;
    Ok(Json(stocks.iter().map(StockResponse::from).collect()))
}

pub async fn get_stock(
    State(service): AppState,
    path: Result<Path<String>, PathRejection>,
) -> ApiResult<StockSummaryResponse> {
    // `let Path(x) = ...` はパターンで包みを剥がして中身を取り出す書き方(分配束縛)。
    let Path(sku) = path?;
    let stocks = service.stocks_of(&Sku::new(sku)?)?;
    Ok(Json(StockSummaryResponse::new(&stocks)))
}

pub async fn list_movements(
    State(service): AppState,
    path: Result<Path<String>, PathRejection>,
) -> ApiResult<Vec<LedgerEntryResponse>> {
    let Path(sku) = path?;
    let entries = service.movements_of(&Sku::new(sku)?)?;
    Ok(Json(
        entries.iter().map(LedgerEntryResponse::from).collect(),
    ))
}

pub async fn receive_stock(
    State(service): AppState,
    path: Result<Path<(String, String)>, PathRejection>,
    payload: Result<Json<ReceiptRequest>, JsonRejection>,
) -> CreatedResult<InventoryChangeResponse> {
    // パスに変数が 2 つあるときは、タプルでまとめて受け取る。
    let Path((warehouse, sku)) = path?;
    let Json(request) = payload?;
    let change = service.receive(
        WarehouseId::new(warehouse)?,
        Sku::new(sku)?,
        Quantity::new(request.quantity),
    )?;
    // 入荷・棚卸は台帳に 1 行「作る」操作とみなして 201 Created を返す。
    Ok((StatusCode::CREATED, Json((&change).into())))
}

pub async fn adjust_stock(
    State(service): AppState,
    path: Result<Path<(String, String)>, PathRejection>,
    payload: Result<Json<AdjustmentRequest>, JsonRejection>,
) -> CreatedResult<InventoryChangeResponse> {
    let Path((warehouse, sku)) = path?;
    let Json(request) = payload?;
    let change = service.adjust(
        WarehouseId::new(warehouse)?,
        Sku::new(sku)?,
        request.delta,
        request.reason.into(),
    )?;
    Ok((StatusCode::CREATED, Json((&change).into())))
}

// ---------------- 受注 ----------------

pub async fn list_orders(State(service): AppState) -> ApiResult<Vec<OrderResponse>> {
    let orders = service.list_orders()?;
    Ok(Json(orders.iter().map(OrderResponse::from).collect()))
}

pub async fn create_order(
    State(service): AppState,
    payload: Result<Json<CreateOrderRequest>, JsonRejection>,
) -> CreatedResult<OrderResponse> {
    let Json(request) = payload?;

    // DTO → ドメイン型への変換。SKU の検証はここで行う。
    // `collect::<Result<Vec<_>, _>>()` は、要素ごとの Result を
    // 「全部 Ok なら Ok(Vec)、1 つでも Err なら最初の Err」にまとめる定番の書き方。
    let lines = request
        .lines
        .into_iter()
        .map(|line| {
            Ok(OrderLine {
                sku: Sku::new(line.sku)?,
                quantity: Quantity::new(line.quantity),
            })
        })
        .collect::<Result<Vec<_>, DomainError>>()?;

    let order = service.create_order(lines)?;
    Ok((StatusCode::CREATED, Json(OrderResponse::from(&order))))
}

pub async fn get_order(
    State(service): AppState,
    path: Result<Path<u64>, PathRejection>,
) -> ApiResult<OrderResponse> {
    let Path(id) = path?;
    let order = service.get_order(OrderId::new(id))?;
    Ok(Json(OrderResponse::from(&order)))
}

pub async fn allocate_order(
    State(service): AppState,
    path: Result<Path<u64>, PathRejection>,
) -> ApiResult<AllocateResponse> {
    let Path(id) = path?;
    let result = service.allocate(OrderId::new(id))?;
    let shortages = match &result.outcome {
        AllocationOutcome::Allocated => None,
        AllocationOutcome::Backordered { shortages } => {
            Some(shortages.iter().map(ShortageResponse::from).collect())
        }
    };
    Ok(Json(AllocateResponse {
        order: OrderResponse::from(&result.order),
        shortages,
    }))
}

pub async fn ship_shipment(
    State(service): AppState,
    path: Result<Path<(u64, u32)>, PathRejection>,
    payload: Result<Json<ShipRequest>, JsonRejection>,
) -> ApiResult<OrderResponse> {
    let Path((id, no)) = path?;
    let Json(request) = payload?;
    let order = service.ship(
        OrderId::new(id),
        ShipmentNo::new(no),
        TrackingNumber::new(request.tracking_number)?,
    )?;
    Ok(Json(OrderResponse::from(&order)))
}

pub async fn cancel_order(
    State(service): AppState,
    path: Result<Path<u64>, PathRejection>,
) -> ApiResult<CancelResponse> {
    let Path(id) = path?;
    let result = service.cancel(OrderId::new(id))?;
    Ok(Json(CancelResponse {
        order: OrderResponse::from(&result.order),
        reallocated_orders: result.reallocated.iter().map(|id| id.value()).collect(),
    }))
}
