//! 起動と組み立て。
//!
//! どの Repository 実装を使うかを決めるのはここだけ。
//! 各層は trait や型エイリアスを通して受け取るので、差し替えの影響がここに閉じる。

use std::sync::Arc;

use order_allocation_demo::api::{self, AppService};
use order_allocation_demo::domain::{Quantity, Sku};
use order_allocation_demo::repository::{InMemoryOrderRepository, InMemoryStockRepository};
use order_allocation_demo::service::Service;

/// `#[tokio::main]` は、async な main を動かすための非同期ランタイムを起動するマクロ。
/// Rust の async は言語機能だけでは動かず、tokio のようなランタイムが必要。
#[tokio::main]
async fn main() {
    let service: AppService = Service::new(
        InMemoryStockRepository::default(),
        InMemoryOrderRepository::default(),
    );
    seed(&service);

    let app = api::router(Arc::new(service));

    let address = "127.0.0.1:3000";
    // 起動時の失敗(ポート使用中など)は回復しようがないので、
    // `expect` でメッセージ付きで終了させる。`expect` は Err なら panic して中身を返す。
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("failed to bind address");
    println!("listening on http://{address}");
    axum::serve(listener, app).await.expect("server error");
}

/// デモ用の初期在庫。README の curl シナリオはこの数量を前提にしている。
fn seed(service: &AppService) {
    let initial = [("APPLE", 10), ("BANANA", 5), ("ORANGE", 2)];
    for (sku, quantity) in initial {
        let sku = Sku::new(sku).expect("seed SKU must be valid");
        service
            .receive(sku, Quantity::new(quantity))
            .expect("seed receipt must succeed");
    }
}
