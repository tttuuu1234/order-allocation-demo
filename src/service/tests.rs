//! サービス層のテスト。
//!
//! ドメイン単体のテストとは違い、ここでは「Repository に保存された状態」が
//! 期待どおりか(失敗時に保存済みデータが変わっていないか)を確かめる。

use super::*;
use crate::domain::OrderStatus;
use crate::repository::{InMemoryOrderRepository, InMemoryStockRepository};

type TestService = Service<InMemoryStockRepository, InMemoryOrderRepository>;

fn sku(value: &str) -> Sku {
    Sku::new(value).unwrap()
}

fn line(value: &str, quantity: u32) -> OrderLine {
    OrderLine {
        sku: sku(value),
        quantity: Quantity::new(quantity),
    }
}

/// APPLE 10 個、BANANA 2 個がある状態のサービスを作る。
fn service() -> TestService {
    let service = Service::new(
        InMemoryStockRepository::default(),
        InMemoryOrderRepository::default(),
    );
    service.receive(sku("APPLE"), Quantity::new(10)).unwrap();
    service.receive(sku("BANANA"), Quantity::new(2)).unwrap();
    service
}

#[test]
fn 入荷で未登録の_sku_は新規作成される() {
    let service = service();
    let stock = service.receive(sku("CHERRY"), Quantity::new(4)).unwrap();
    assert_eq!(stock.on_hand(), Quantity::new(4));
    assert_eq!(service.get_stock(&sku("CHERRY")).unwrap(), stock);
}

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

#[test]
fn 入荷_注文_引当_出荷の一連の流れ() {
    let service = service();
    let order = service.create_order(vec![line("APPLE", 3)]).unwrap();

    let allocated = service.allocate(order.id()).unwrap();
    assert_eq!(allocated.status(), OrderStatus::Allocated);
    let apple = service.get_stock(&sku("APPLE")).unwrap();
    assert_eq!(apple.reserved(), Quantity::new(3));
    assert_eq!(apple.available(), Quantity::new(7));

    let shipped = service.ship(order.id()).unwrap();
    assert_eq!(shipped.status(), OrderStatus::Shipped);
    let apple = service.get_stock(&sku("APPLE")).unwrap();
    assert_eq!(apple.on_hand(), Quantity::new(7));
    assert_eq!(apple.reserved(), Quantity::ZERO);
}

#[test]
fn 引当失敗時は保存済みの在庫も注文状態も変わらない() {
    let service = service();
    // APPLE は足りるが BANANA が足りない
    let order = service
        .create_order(vec![line("APPLE", 3), line("BANANA", 5)])
        .unwrap();
    let stocks_before = service.list_stocks().unwrap();

    let result = service.allocate(order.id());

    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InsufficientStock { .. }))
    ));
    assert_eq!(service.list_stocks().unwrap(), stocks_before);
    assert_eq!(
        service.get_order(order.id()).unwrap().status(),
        OrderStatus::Pending
    );
}

#[test]
fn 在庫を取り合うと後の注文は引当できない() {
    let service = service();
    let first = service.create_order(vec![line("BANANA", 2)]).unwrap();
    let second = service.create_order(vec![line("BANANA", 1)]).unwrap();

    service.allocate(first.id()).unwrap();
    let result = service.allocate(second.id());

    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InsufficientStock { .. }))
    ));
}

#[test]
fn 引当済みのキャンセルで在庫が戻り_別の注文が引当できる() {
    let service = service();
    let first = service.create_order(vec![line("BANANA", 2)]).unwrap();
    let second = service.create_order(vec![line("BANANA", 2)]).unwrap();
    service.allocate(first.id()).unwrap();

    let cancelled = service.cancel(first.id()).unwrap();
    assert_eq!(cancelled.status(), OrderStatus::Cancelled);
    assert_eq!(
        service.get_stock(&sku("BANANA")).unwrap().available(),
        Quantity::new(2)
    );

    service.allocate(second.id()).unwrap();
}

#[test]
fn 不正な状態遷移はエラーで保存済みの状態は変わらない() {
    let service = service();
    let order = service.create_order(vec![line("APPLE", 1)]).unwrap();

    // Pending のまま出荷
    let result = service.ship(order.id());
    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InvalidTransition { .. }))
    ));
    assert_eq!(
        service.get_order(order.id()).unwrap().status(),
        OrderStatus::Pending
    );

    // 出荷済みのキャンセル
    service.allocate(order.id()).unwrap();
    service.ship(order.id()).unwrap();
    let stocks_before = service.list_stocks().unwrap();
    let result = service.cancel(order.id());
    assert!(matches!(
        result,
        Err(ServiceError::Domain(DomainError::InvalidTransition { .. }))
    ));
    assert_eq!(
        service.get_order(order.id()).unwrap().status(),
        OrderStatus::Shipped
    );
    assert_eq!(service.list_stocks().unwrap(), stocks_before);
}

#[test]
fn 存在しない注文はエラー() {
    let service = service();
    assert_eq!(
        service.allocate(OrderId::new(999)),
        Err(ServiceError::OrderNotFound(OrderId::new(999)))
    );
}
