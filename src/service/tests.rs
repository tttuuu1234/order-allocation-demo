//! サービス層のテスト。
//!
//! ドメイン単体のテストとは違い、ここでは「Repository に保存された状態」が
//! 期待どおりか(失敗時に保存済みデータが変わっていないか、台帳と在庫が一致するか)を確かめる。

use super::*;
use crate::domain::{MovementReason, PreferSingleWarehouse, ShipmentStatus};
use crate::repository::{InMemoryInventoryRepository, InMemoryOrderRepository};

type TestService = Service<InMemoryInventoryRepository, InMemoryOrderRepository>;

fn wh(id: &str) -> WarehouseId {
    WarehouseId::new(id).unwrap()
}

fn sku(value: &str) -> Sku {
    Sku::new(value).unwrap()
}

fn line(value: &str, quantity: u32) -> OrderLine {
    OrderLine {
        sku: sku(value),
        quantity: Quantity::new(quantity),
    }
}

fn tracking(value: &str) -> TrackingNumber {
    TrackingNumber::new(value).unwrap()
}

/// TOKYO: APPLE 10, BANANA 5 / OSAKA: APPLE 5, ORANGE 2 の状態のサービスを作る。
fn service() -> TestService {
    let service = Service::new(
        InMemoryInventoryRepository::default(),
        InMemoryOrderRepository::default(),
        PreferSingleWarehouse::new(vec![wh("TOKYO"), wh("OSAKA")]),
    );
    for (warehouse, sku_value, quantity) in [
        ("TOKYO", "APPLE", 10),
        ("TOKYO", "BANANA", 5),
        ("OSAKA", "APPLE", 5),
        ("OSAKA", "ORANGE", 2),
    ] {
        service
            .receive(wh(warehouse), sku(sku_value), Quantity::new(quantity))
            .unwrap();
    }
    service
}

fn stock_at(service: &TestService, warehouse: &str, sku_value: &str) -> Stock {
    service
        .stocks_of(&sku(sku_value))
        .unwrap()
        .into_iter()
        .find(|s| s.warehouse() == &wh(warehouse))
        .unwrap()
}

/// 台帳の増減を合計すると、在庫の on_hand と一致すること(受払台帳の最重要の性質)。
fn assert_ledger_matches_stocks(service: &TestService) {
    for stock in service.list_stocks().unwrap() {
        let total: i64 = service
            .movements_of(stock.sku())
            .unwrap()
            .iter()
            .filter(|e| &e.movement.warehouse == stock.warehouse())
            .map(|e| e.movement.delta)
            .sum();
        assert_eq!(
            total,
            i64::from(stock.on_hand().value()),
            "{} at {}",
            stock.sku(),
            stock.warehouse()
        );
    }
}

// ---------------- 在庫・台帳 ----------------

#[test]
fn 入荷で未登録の倉庫と_sku_は新規作成され台帳に記録される() {
    let service = service();
    let change = service
        .receive(wh("FUKUOKA"), sku("CHERRY"), Quantity::new(4))
        .unwrap();

    assert_eq!(change.stock.on_hand(), Quantity::new(4));
    assert_eq!(change.entry.movement.delta, 4);
    assert_eq!(change.entry.movement.reason, MovementReason::Receipt);
    assert_eq!(stock_at(&service, "FUKUOKA", "CHERRY"), change.stock);
}

#[test]
fn 棚卸調整は理由付きで台帳に残る() {
    let service = service();
    let change = service
        .adjust(wh("TOKYO"), sku("APPLE"), -2, AdjustmentReason::Damaged)
        .unwrap();

    assert_eq!(change.stock.on_hand(), Quantity::new(8));
    let last = service.movements_of(&sku("APPLE")).unwrap().pop().unwrap();
    assert_eq!(last.movement.delta, -2);
    assert_eq!(
        last.movement.reason,
        MovementReason::Adjustment(AdjustmentReason::Damaged)
    );
    assert_ledger_matches_stocks(&service);
}

#[test]
fn 在庫レコードが無い倉庫の棚卸調整はエラー() {
    let service = service();
    let result = service.adjust(wh("OSAKA"), sku("BANANA"), 1, AdjustmentReason::Found);
    assert!(matches!(
        result,
        Err(ServiceError::StockNotFound {
            warehouse: Some(_),
            ..
        })
    ));
}

#[test]
fn 引当済みを下回る棚卸調整はエラーで_在庫も台帳も変わらない() {
    let service = service();
    let order = service.create_order(vec![line("BANANA", 4)]).unwrap();
    service.allocate(order.id()).unwrap();
    let ledger_before = service.movements_of(&sku("BANANA")).unwrap();

    let result = service.adjust(wh("TOKYO"), sku("BANANA"), -2, AdjustmentReason::Lost);

    assert!(matches!(
        result,
        Err(ServiceError::Domain(
            DomainError::AdjustmentBelowReserved { .. }
        ))
    ));
    assert_eq!(
        stock_at(&service, "TOKYO", "BANANA").on_hand(),
        Quantity::new(5)
    );
    assert_eq!(service.movements_of(&sku("BANANA")).unwrap(), ledger_before);
}

// ---------------- 受注 ----------------

#[test]
fn 注文時に同一_sku_は合算されて保存される() {
    let service = service();
    let order = service
        .create_order(vec![line("APPLE", 1), line("APPLE", 2)])
        .unwrap();

    let saved = service.get_order(order.id()).unwrap();
    assert_eq!(saved.lines(), &[line("APPLE", 3)]);
    assert_eq!(saved.status(), OrderStatus::Pending);
}

#[test]
fn 未知の_sku_を含む注文はエラーで_未知の_sku_はすべて返る() {
    let service = service();
    let result = service.create_order(vec![line("APPLE", 1), line("X", 1), line("Y", 1)]);

    assert_eq!(
        result,
        Err(ServiceError::UnknownSkus(vec![sku("X"), sku("Y")]))
    );
    assert!(service.list_orders().unwrap().is_empty());
}

#[test]
fn 検証に失敗した注文の_id_は欠番になる() {
    let service = service();
    let _ = service.create_order(vec![line("X", 1)]); // 失敗して ID 1 が欠番に
    let order = service.create_order(vec![line("APPLE", 1)]).unwrap();
    assert_eq!(order.id(), OrderId::new(2));
}

// ---------------- 引当・出荷 ----------------

#[test]
fn 倉庫をまたぐ注文は出荷が分かれ_一つずつ出荷できる() {
    let service = service();
    // BANANA は TOKYO にしか無く、ORANGE は OSAKA にしか無い
    let order = service
        .create_order(vec![line("BANANA", 2), line("ORANGE", 1)])
        .unwrap();

    let result = service.allocate(order.id()).unwrap();
    assert_eq!(result.outcome, AllocationOutcome::Allocated);
    assert_eq!(result.order.shipments().len(), 2);

    let order = service
        .ship(order.id(), ShipmentNo::new(1), tracking("TRK-1"))
        .unwrap();
    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
    assert_eq!(
        stock_at(&service, "TOKYO", "BANANA").on_hand(),
        Quantity::new(3)
    );
    assert_eq!(
        stock_at(&service, "OSAKA", "ORANGE").on_hand(),
        Quantity::new(2)
    );

    let order = service
        .ship(order.id(), ShipmentNo::new(2), tracking("TRK-2"))
        .unwrap();
    assert_eq!(order.status(), OrderStatus::Shipped);
    assert_eq!(
        order.shipments()[1].status(),
        &ShipmentStatus::Shipped {
            tracking_number: tracking("TRK-2")
        }
    );
    assert_eq!(
        stock_at(&service, "OSAKA", "ORANGE").on_hand(),
        Quantity::new(1)
    );
    assert_ledger_matches_stocks(&service);
}

#[test]
fn 在庫不足なら入荷待ちになり_保存済みの在庫は変わらない() {
    let service = service();
    // APPLE は足りるが ORANGE が足りない
    let order = service
        .create_order(vec![line("APPLE", 3), line("ORANGE", 5)])
        .unwrap();
    let stocks_before = service.list_stocks().unwrap();

    let result = service.allocate(order.id()).unwrap();

    assert!(matches!(
        result.outcome,
        AllocationOutcome::Backordered { .. }
    ));
    assert_eq!(service.list_stocks().unwrap(), stocks_before);
    let saved = service.get_order(order.id()).unwrap();
    assert_eq!(saved.status(), OrderStatus::Backordered);
    assert!(saved.shipments().is_empty());
}

#[test]
fn 入荷すると入荷待ちの注文が先着順に自動で引当される() {
    let service = service();
    let first = service.create_order(vec![line("ORANGE", 3)]).unwrap();
    let second = service.create_order(vec![line("ORANGE", 2)]).unwrap();
    service.allocate(first.id()).unwrap(); // 2 個しか無いので入荷待ち
    service.allocate(second.id()).unwrap(); // ちょうど 2 個なので引当される
    assert_eq!(
        service.get_order(first.id()).unwrap().status(),
        OrderStatus::Backordered
    );

    // 1 個入っても 3 個には足りない
    let change = service
        .receive(wh("OSAKA"), sku("ORANGE"), Quantity::new(1))
        .unwrap();
    assert!(change.reallocated.is_empty());

    // さらに 2 個入ると 3 個そろう
    let change = service
        .receive(wh("OSAKA"), sku("ORANGE"), Quantity::new(2))
        .unwrap();
    assert_eq!(change.reallocated, vec![first.id()]);
    assert_eq!(
        service.get_order(first.id()).unwrap().status(),
        OrderStatus::Allocated
    );
    assert_eq!(
        stock_at(&service, "OSAKA", "ORANGE").available(),
        Quantity::ZERO
    );
}

#[test]
fn キャンセルで解放された在庫は入荷待ちの注文に回る() {
    let service = service();
    let first = service.create_order(vec![line("ORANGE", 2)]).unwrap();
    let second = service.create_order(vec![line("ORANGE", 2)]).unwrap();
    service.allocate(first.id()).unwrap();
    service.allocate(second.id()).unwrap(); // 入荷待ち

    let result = service.cancel(first.id()).unwrap();

    assert_eq!(result.order.status(), OrderStatus::Cancelled);
    assert_eq!(result.reallocated, vec![second.id()]);
    assert_eq!(
        service.get_order(second.id()).unwrap().status(),
        OrderStatus::Allocated
    );
}

#[test]
fn 入荷待ちの注文自身をキャンセルしても再引当の対象にならない() {
    let service = service();
    let order = service.create_order(vec![line("ORANGE", 9)]).unwrap();
    service.allocate(order.id()).unwrap();

    let result = service.cancel(order.id()).unwrap();

    assert_eq!(result.order.status(), OrderStatus::Cancelled);
    assert!(result.reallocated.is_empty());
}

// ---------------- 不正な操作 ----------------

#[test]
fn 不正な状態遷移はエラーで保存済みの状態は変わらない() {
    let service = service();
    let order = service
        .create_order(vec![line("BANANA", 1), line("ORANGE", 1)])
        .unwrap();

    // Pending のまま出荷
    let result = service.ship(order.id(), ShipmentNo::new(1), tracking("T"));
    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InvalidTransition { .. }))
    ));
    assert_eq!(
        service.get_order(order.id()).unwrap().status(),
        OrderStatus::Pending
    );

    // 一部出荷済みのキャンセル
    service.allocate(order.id()).unwrap();
    service
        .ship(order.id(), ShipmentNo::new(1), tracking("T"))
        .unwrap();
    let stocks_before = service.list_stocks().unwrap();
    let result = service.cancel(order.id());
    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InvalidTransition { .. }))
    ));
    assert_eq!(
        service.get_order(order.id()).unwrap().status(),
        OrderStatus::PartiallyShipped
    );
    assert_eq!(service.list_stocks().unwrap(), stocks_before);
}

#[test]
fn 出荷済みの出荷を再度出荷するとエラーで台帳も増えない() {
    let service = service();
    let order = service.create_order(vec![line("APPLE", 1)]).unwrap();
    service.allocate(order.id()).unwrap();
    service
        .ship(order.id(), ShipmentNo::new(1), tracking("T"))
        .unwrap();
    let ledger_before = service.movements_of(&sku("APPLE")).unwrap();

    let result = service.ship(order.id(), ShipmentNo::new(1), tracking("T"));

    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InvalidTransition { .. }))
    ));
    assert_eq!(service.movements_of(&sku("APPLE")).unwrap(), ledger_before);
}

#[test]
fn 存在しない注文はエラー() {
    let service = service();
    assert_eq!(
        service.allocate(OrderId::new(999)),
        Err(ServiceError::OrderNotFound(OrderId::new(999)))
    );
}
