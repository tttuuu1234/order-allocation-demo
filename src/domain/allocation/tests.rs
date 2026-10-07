//! 受注と在庫をまたぐ操作のテスト。
//! 特に「失敗・入荷待ちのときに在庫が一切変わらないこと」を確かめる。

use super::*;
use crate::domain::{OrderLine, PreferSingleWarehouse, Quantity};

fn wh(id: &str) -> WarehouseId {
    WarehouseId::new(id).unwrap()
}

fn sku(value: &str) -> Sku {
    Sku::new(value).unwrap()
}

fn stock(warehouse: &str, sku_value: &str, on_hand: u32) -> Stock {
    let mut stock = Stock::new(wh(warehouse), sku(sku_value));
    let _ = stock.receive(Quantity::new(on_hand)).unwrap();
    stock
}

fn order(id: u64, lines: &[(&str, u32)]) -> Order {
    let lines = lines
        .iter()
        .map(|(s, q)| OrderLine {
            sku: sku(s),
            quantity: Quantity::new(*q),
        })
        .collect();
    Order::new(OrderId::new(id), lines).unwrap()
}

fn strategy() -> PreferSingleWarehouse {
    PreferSingleWarehouse::new(vec![wh("TOKYO"), wh("OSAKA")])
}

fn tracking() -> TrackingNumber {
    TrackingNumber::new("TRK-1").unwrap()
}

/// TOKYO: APPLE 10 / OSAKA: APPLE 5, ORANGE 2
fn stocks() -> Vec<Stock> {
    vec![
        stock("TOKYO", "APPLE", 10),
        stock("OSAKA", "APPLE", 5),
        stock("OSAKA", "ORANGE", 2),
    ]
}

#[test]
fn 全明細を確保できれば引当され_倉庫ごとの在庫が押さえられる() {
    // APPLE 12 個は TOKYO 10 + OSAKA 2 に分かれる
    let mut order = order(1, &[("APPLE", 12), ("ORANGE", 1)]);
    let mut stocks = stocks();

    let outcome = allocate(&mut order, &mut stocks, &strategy()).unwrap();

    assert_eq!(outcome, AllocationOutcome::Allocated);
    assert_eq!(order.status(), OrderStatus::Allocated);
    assert_eq!(stocks[0].reserved(), Quantity::new(10)); // TOKYO APPLE
    assert_eq!(stocks[1].reserved(), Quantity::new(2)); // OSAKA APPLE
    assert_eq!(stocks[2].reserved(), Quantity::new(1)); // OSAKA ORANGE
}

#[test]
fn 一部でも不足すれば在庫は一切変わらず入荷待ちになり_不足は全明細ぶん返る() {
    let mut order = order(1, &[("APPLE", 20), ("ORANGE", 3)]);
    let mut stocks = stocks();
    let stocks_before = stocks.clone();

    let outcome = allocate(&mut order, &mut stocks, &strategy()).unwrap();

    assert_eq!(
        outcome,
        AllocationOutcome::Backordered {
            shortages: vec![
                Shortage {
                    sku: sku("APPLE"),
                    requested: Quantity::new(20),
                    available: Quantity::new(15),
                },
                Shortage {
                    sku: sku("ORANGE"),
                    requested: Quantity::new(3),
                    available: Quantity::new(2),
                },
            ]
        }
    );
    assert_eq!(stocks, stocks_before);
    assert_eq!(order.status(), OrderStatus::Backordered);
    assert!(order.shipments().is_empty());
}

#[test]
fn 引当済みの注文を再度引き当てると状態エラーで在庫は変わらない() {
    let mut order = order(1, &[("APPLE", 3)]);
    let mut stocks = stocks();
    allocate(&mut order, &mut stocks, &strategy()).unwrap();
    let stocks_before = stocks.clone();

    let result = allocate(&mut order, &mut stocks, &strategy());

    assert!(matches!(result, Err(DomainError::InvalidTransition { .. })));
    assert_eq!(stocks, stocks_before);
}

#[test]
fn 出荷はその出荷の倉庫の在庫だけを減らし_台帳の行を返す() {
    let mut order = order(1, &[("APPLE", 12)]);
    let mut stocks = stocks();
    allocate(&mut order, &mut stocks, &strategy()).unwrap(); // TOKYO 10 + OSAKA 2

    let movements = ship(&mut order, ShipmentNo::new(2), tracking(), &mut stocks).unwrap();

    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
    assert_eq!(movements.len(), 1);
    assert_eq!(movements[0].warehouse, wh("OSAKA"));
    assert_eq!(movements[0].delta, -2);
    // OSAKA は減り、TOKYO は引当のまま
    assert_eq!(stocks[1].on_hand(), Quantity::new(3));
    assert_eq!(stocks[1].reserved(), Quantity::ZERO);
    assert_eq!(stocks[0].on_hand(), Quantity::new(10));
    assert_eq!(stocks[0].reserved(), Quantity::new(10));
}

#[test]
fn 引当前の出荷は状態エラーで在庫は変わらない() {
    let mut order = order(1, &[("APPLE", 3)]);
    let mut stocks = stocks();
    let stocks_before = stocks.clone();

    let result = ship(&mut order, ShipmentNo::new(1), tracking(), &mut stocks);

    assert!(matches!(
        result,
        Err(DomainError::InvalidTransition {
            from: OrderStatus::Pending,
            ..
        })
    ));
    assert_eq!(stocks, stocks_before);
}

#[test]
fn 引当済みのキャンセルは全倉庫の引当を解放する() {
    let mut order = order(1, &[("APPLE", 12), ("ORANGE", 1)]);
    let mut stocks = stocks();
    allocate(&mut order, &mut stocks, &strategy()).unwrap();

    cancel(&mut order, &mut stocks).unwrap();

    assert_eq!(order.status(), OrderStatus::Cancelled);
    assert!(stocks.iter().all(|s| s.reserved() == Quantity::ZERO));
}

#[test]
fn 一部出荷済みはキャンセルできず在庫も変わらない() {
    let mut order = order(1, &[("APPLE", 12)]);
    let mut stocks = stocks();
    allocate(&mut order, &mut stocks, &strategy()).unwrap();
    let _ = ship(&mut order, ShipmentNo::new(1), tracking(), &mut stocks).unwrap();
    let stocks_before = stocks.clone();

    let result = cancel(&mut order, &mut stocks);

    assert!(matches!(result, Err(DomainError::InvalidTransition { .. })));
    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
    assert_eq!(stocks, stocks_before);
}

#[test]
fn 再引当は先着順で_足りない注文は待ちのまま後ろの注文が追い越す() {
    // ORANGE は OSAKA に 2 個だけ。3 件とも入荷待ちにしておく
    let mut orders = vec![
        order(1, &[("ORANGE", 3)]),
        order(2, &[("ORANGE", 2)]),
        order(3, &[("ORANGE", 1)]),
    ];
    for o in &mut orders {
        o.backorder().unwrap();
    }
    let mut stocks = stocks();

    let allocated = reallocate_backorders(&mut orders, &mut stocks, &strategy()).unwrap();

    // 1 番は 3 個必要なので待ちのまま。2 番が 2 個を取り、3 番はもう残っていない
    assert_eq!(allocated, vec![OrderId::new(2)]);
    assert_eq!(orders[0].status(), OrderStatus::Backordered);
    assert_eq!(orders[1].status(), OrderStatus::Allocated);
    assert_eq!(orders[2].status(), OrderStatus::Backordered);
    assert_eq!(stocks[2].available(), Quantity::ZERO);
}
