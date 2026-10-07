//! HTTP 層。リクエストの受け取り、DTO との変換、エラーの HTTP 化だけを担う。
//!
//! 業務ルールはここに書かない。ここにあるのは「JSON とドメイン型の翻訳」と
//! 「どのエラーを何番のステータスにするか」という HTTP の都合だけ。
//!
//! - `dto.rs`      リクエスト/レスポンスの JSON の形
//! - `error.rs`    エラー → HTTP ステータスと `{"code","message"}`
//! - `handlers.rs` 各エンドポイントの処理

mod dto;
mod error;
mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::repository::{InMemoryInventoryRepository, InMemoryOrderRepository};
use crate::service::Service;

/// アプリで使うサービスの具体型。
///
/// ハンドラをジェネリクスで書くこともできるが、読みやすさを優先して型を固定した。
/// SQLite に差し替えるときは、この 1 行(と main.rs の組み立て)を変えればよい。
pub type AppService = Service<InMemoryInventoryRepository, InMemoryOrderRepository>;

/// ルーティング。
///
/// `Arc` は複数の所有者で共有できるスマートポインタ(参照カウント)。
/// axum はリクエストごとに State を複製するので、中身を共有するために Arc で包む。
/// Swift のクラス参照や Kotlin のオブジェクト参照の共有に近い。
///
/// パスの `{sku}` は axum 0.8 の書き方(0.7 までは `:sku`)。
pub fn router(service: Arc<AppService>) -> Router {
    use handlers::*;

    Router::new()
        // 在庫の照会(全倉庫を横断)
        .route("/stocks", get(list_stocks))
        .route("/stocks/{sku}", get(get_stock))
        .route("/stocks/{sku}/movements", get(list_movements))
        // 在庫の変更(倉庫を指定)
        .route(
            "/warehouses/{warehouse}/stocks/{sku}/receipts",
            post(receive_stock),
        )
        .route(
            "/warehouses/{warehouse}/stocks/{sku}/adjustments",
            post(adjust_stock),
        )
        // 受注
        .route("/orders", get(list_orders).post(create_order))
        .route("/orders/{id}", get(get_order))
        .route("/orders/{id}/allocate", post(allocate_order))
        .route("/orders/{id}/cancel", post(cancel_order))
        .route("/orders/{id}/shipments/{no}/ship", post(ship_shipment))
        .with_state(service)
}
