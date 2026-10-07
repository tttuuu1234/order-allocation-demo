//! HTTP 層。リクエストの受け取り、DTO との変換、エラーの HTTP 化だけを担う。
//!
//! 業務ルールはここに書かない。ここにあるのは「JSON とドメイン型の翻訳」と
//! 「どのエラーを何番のステータスにするか」という HTTP の都合だけ。

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::domain::{
    DomainError, Order, OrderId, OrderLine, OrderStatus, Quantity, Shortage, Sku, Stock,
};
use crate::repository::{InMemoryOrderRepository, InMemoryStockRepository};
use crate::service::{Service, ServiceError};

/// アプリで使うサービスの具体型。
///
/// ハンドラをジェネリクスで書くこともできるが、読みやすさを優先して型を固定した。
/// SQLite に差し替えるときは、この 1 行(と main.rs の組み立て)を変えればよい。
pub type AppService = Service<InMemoryStockRepository, InMemoryOrderRepository>;

/// ルーティング。
///
/// `Arc` は複数の所有者で共有できるスマートポインタ(参照カウント)。
/// axum はリクエストごとに State を複製するので、中身を共有するために Arc で包む。
/// Swift のクラス参照や Kotlin のオブジェクト参照の共有に近い。
///
/// パスの `{sku}` は axum 0.8 の書き方(0.7 までは `:sku`)。
pub fn router(service: Arc<AppService>) -> Router {
    Router::new()
        .route("/stocks", get(list_stocks))
        .route("/stocks/{sku}", get(get_stock))
        .route("/stocks/{sku}/receipts", post(receive_stock))
        .route("/orders", get(list_orders).post(create_order))
        .route("/orders/{id}", get(get_order))
        .route("/orders/{id}/allocate", post(allocate_order))
        .route("/orders/{id}/ship", post(ship_order))
        .route("/orders/{id}/cancel", post(cancel_order))
        .with_state(service)
}

// ---------------- リクエスト DTO ----------------
//
// DTO をドメイン型と分けているのは、JSON の形(API の契約)とドメインの内部表現を
// 独立に変えられるようにするため。ドメイン型に serde を付けない理由でもある。

/// `#[derive(Deserialize)]` で、JSON からこの構造体への変換コードが自動生成される。
#[derive(Debug, Deserialize)]
pub struct ReceiptRequest {
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub lines: Vec<OrderLineRequest>,
}

#[derive(Debug, Deserialize)]
pub struct OrderLineRequest {
    pub sku: String,
    pub quantity: u32,
}

// ---------------- レスポンス DTO ----------------

#[derive(Debug, Serialize)]
pub struct StockResponse {
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
            sku: stock.sku().to_string(),
            on_hand: stock.on_hand().value(),
            reserved: stock.reserved().value(),
            available: stock.available().value(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub id: u64,
    pub status: String,
    pub lines: Vec<OrderLineResponse>,
}

#[derive(Debug, Serialize)]
pub struct OrderLineResponse {
    pub sku: String,
    pub quantity: u32,
}

impl From<&Order> for OrderResponse {
    fn from(order: &Order) -> Self {
        OrderResponse {
            id: order.id().value(),
            status: status_name(order.status()).to_string(),
            lines: order
                .lines()
                .iter()
                .map(|line| OrderLineResponse {
                    sku: line.sku.to_string(),
                    quantity: line.quantity.value(),
                })
                .collect(),
        }
    }
}

/// JSON に出す状態名。ドメインの Display に頼らず API の契約としてここに明記する。
fn status_name(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "Pending",
        OrderStatus::Allocated => "Allocated",
        OrderStatus::Shipped => "Shipped",
        OrderStatus::Cancelled => "Cancelled",
    }
}

// ---------------- エラー ----------------

/// エラーレスポンス。`{"code": "...", "message": "..."}` の形で返す。
///
/// 在庫不足のときだけ、どの SKU がいくつ足りないかを `shortages` に入れる。
/// `skip_serializing_if` で、None のときはキーごと出力しない。
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
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

/// ハンドラが返すエラー。ステータスコードと本文の組。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
                shortages: None,
            },
        }
    }
}

// `IntoResponse` を実装すると、ハンドラの戻り値としてそのまま返せるようになる。
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// サービスのエラーを HTTP に変換する。どのエラーを何番にするかの対応表はここだけにある。
impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        // メッセージは match で値を取り出す前に作っておく(match で中身をムーブするため)。
        let message = error.to_string();
        match error {
            ServiceError::Domain(domain_error) => from_domain_error(domain_error, message),
            ServiceError::StockNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "stock_not_found", message)
            }
            ServiceError::OrderNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "order_not_found", message)
            }
            // 注文の中の SKU が未知なのは、URL のリソースが無い(404)のではなく
            // 送られてきた内容が不正という扱いなので 422 にする。
            ServiceError::UnknownSkus(_) => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_sku", message)
            }
            ServiceError::Repository(_) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
            }
        }
    }
}

fn from_domain_error(error: DomainError, message: String) -> ApiError {
    match error {
        DomainError::EmptySku
        | DomainError::ZeroQuantity { .. }
        | DomainError::EmptyOrderLines
        | DomainError::QuantityOverflow { .. } => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", message)
        }
        DomainError::InsufficientStock { shortages } => {
            let mut api_error = ApiError::new(StatusCode::CONFLICT, "insufficient_stock", message);
            api_error.body.shortages = Some(shortages.iter().map(ShortageResponse::from).collect());
            api_error
        }
        DomainError::InvalidTransition { .. } => {
            ApiError::new(StatusCode::CONFLICT, "invalid_transition", message)
        }
        // 内部不整合は利用者のせいではないので 500。
        DomainError::ReservedUnderflow { .. } | DomainError::StockMissing { .. } => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
        }
    }
}

/// JSON が壊れている・型が合わないなど、axum が本文を読めなかったとき。
///
/// そのままだと axum はプレーンテキストでエラーを返すので、
/// 他のエラーと同じ JSON 形式にそろえるために変換する。
/// ステータスは axum の判断(構文エラーは 400、型の不一致は 422 など)をそのまま使う。
impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::new(rejection.status(), "invalid_json", rejection.body_text())
    }
}

/// パスの値が読めなかったとき(例: `/orders/abc`)。
impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            rejection.body_text(),
        )
    }
}

// ---------------- ハンドラ ----------------
//
// 引数の `Result<Json<T>, JsonRejection>` は「本文の読み取りに失敗しても
// ハンドラまで来させる」ための書き方。中で `?` を使うと、上の `impl From` により
// ApiError に変換されて返る。
//
// サービスのメソッドは同期関数(ロック中に .await しない短い処理)なので、
// async ハンドラからそのまま呼んでいる。重い処理なら spawn_blocking を検討する。

async fn list_stocks(
    State(service): State<Arc<AppService>>,
) -> Result<Json<Vec<StockResponse>>, ApiError> {
    let stocks = service.list_stocks()?;
    Ok(Json(stocks.iter().map(StockResponse::from).collect()))
}

async fn get_stock(
    State(service): State<Arc<AppService>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<StockResponse>, ApiError> {
    // `let Path(x) = ...` はパターンで包みを剥がして中身を取り出す書き方(分配束縛)。
    let Path(sku) = path?;
    // DomainError → ServiceError → ApiError と、`From` を 2 段たどって変換している。
    let sku = Sku::new(sku).map_err(ServiceError::from)?;
    let stock = service.get_stock(&sku)?;
    Ok(Json(StockResponse::from(&stock)))
}

async fn receive_stock(
    State(service): State<Arc<AppService>>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<ReceiptRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<StockResponse>), ApiError> {
    let Path(sku) = path?;
    let Json(request) = payload?;
    let sku = Sku::new(sku).map_err(ServiceError::from)?;
    let stock = service.receive(sku, Quantity::new(request.quantity))?;
    // 入荷は「入荷記録」を作る操作とみなして 201 Created を返す。
    Ok((StatusCode::CREATED, Json(StockResponse::from(&stock))))
}

async fn list_orders(
    State(service): State<Arc<AppService>>,
) -> Result<Json<Vec<OrderResponse>>, ApiError> {
    let orders = service.list_orders()?;
    Ok(Json(orders.iter().map(OrderResponse::from).collect()))
}

async fn create_order(
    State(service): State<Arc<AppService>>,
    payload: Result<Json<CreateOrderRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<OrderResponse>), ApiError> {
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
        .collect::<Result<Vec<_>, DomainError>>()
        .map_err(ServiceError::from)?;

    let order = service.create_order(lines)?;
    Ok((StatusCode::CREATED, Json(OrderResponse::from(&order))))
}

async fn get_order(
    State(service): State<Arc<AppService>>,
    path: Result<Path<u64>, PathRejection>,
) -> Result<Json<OrderResponse>, ApiError> {
    let Path(id) = path?;
    let order = service.get_order(OrderId::new(id))?;
    Ok(Json(OrderResponse::from(&order)))
}

async fn allocate_order(
    State(service): State<Arc<AppService>>,
    path: Result<Path<u64>, PathRejection>,
) -> Result<Json<OrderResponse>, ApiError> {
    let Path(id) = path?;
    let order = service.allocate(OrderId::new(id))?;
    Ok(Json(OrderResponse::from(&order)))
}

async fn ship_order(
    State(service): State<Arc<AppService>>,
    path: Result<Path<u64>, PathRejection>,
) -> Result<Json<OrderResponse>, ApiError> {
    let Path(id) = path?;
    let order = service.ship(OrderId::new(id))?;
    Ok(Json(OrderResponse::from(&order)))
}

async fn cancel_order(
    State(service): State<Arc<AppService>>,
    path: Result<Path<u64>, PathRejection>,
) -> Result<Json<OrderResponse>, ApiError> {
    let Path(id) = path?;
    let order = service.cancel(OrderId::new(id))?;
    Ok(Json(OrderResponse::from(&order)))
}
