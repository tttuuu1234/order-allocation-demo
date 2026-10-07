//! Order 単体のテスト。状態遷移の正常系・異常系と、明細の合算。

use super::*;
use crate::domain::ShipmentStatus;

fn line(sku: &str, quantity: u32) -> OrderLine {
    OrderLine {
        sku: Sku::new(sku).unwrap(),
        quantity: Quantity::new(quantity),
    }
}

fn planned(warehouse: &str, sku: &str, quantity: u32) -> PlannedAllocation {
    PlannedAllocation {
        warehouse: WarehouseId::new(warehouse).unwrap(),
        sku: Sku::new(sku).unwrap(),
        quantity: Quantity::new(quantity),
    }
}

fn tracking() -> TrackingNumber {
    TrackingNumber::new("TRK-1").unwrap()
}

/// APPLE 3 個・ORANGE 1 個の Pending の注文
fn pending_order() -> Order {
    Order::new(OrderId::new(1), vec![line("APPLE", 3), line("ORANGE", 1)]).unwrap()
}

/// APPLE は TOKYO、ORANGE は OSAKA から出す(出荷が 2 つに分かれる)引当済みの注文
fn split_order() -> Order {
    let mut order = pending_order();
    order
        .allocate(vec![
            planned("TOKYO", "APPLE", 3),
            planned("OSAKA", "ORANGE", 1),
        ])
        .unwrap();
    order
}

// ---- 生成と合算 ----

#[test]
fn 同一_sku_の明細は合算され順序は保たれる() {
    let order = Order::new(
        OrderId::new(1),
        vec![line("APPLE", 2), line("BANANA", 1), line("APPLE", 3)],
    )
    .unwrap();
    assert_eq!(order.lines(), &[line("APPLE", 5), line("BANANA", 1)]);
}

#[test]
fn 明細が空の注文はエラー() {
    let result = Order::new(OrderId::new(1), vec![]);
    assert_eq!(result, Err(DomainError::EmptyOrderLines));
}

#[test]
fn 数量0の明細はエラー() {
    let result = Order::new(OrderId::new(1), vec![line("APPLE", 1), line("BANANA", 0)]);
    assert!(matches!(result, Err(DomainError::ZeroQuantity { sku }) if sku.as_str() == "BANANA"));
}

#[test]
fn 新しい注文は_pending_で出荷は無い() {
    let order = pending_order();
    assert_eq!(order.status(), OrderStatus::Pending);
    assert!(order.shipments().is_empty());
}

// ---- 引当と出荷 ----

#[test]
fn 引当で倉庫ごとに出荷が作られる() {
    let order = split_order();
    assert_eq!(order.status(), OrderStatus::Allocated);
    let shipments = order.shipments();
    assert_eq!(shipments.len(), 2);
    assert_eq!(shipments[0].no(), ShipmentNo::new(1));
    assert_eq!(shipments[0].warehouse().as_str(), "TOKYO");
    assert_eq!(shipments[1].warehouse().as_str(), "OSAKA");
}

#[test]
fn 同じ倉庫の計画は一つの出荷にまとまる() {
    let mut order = pending_order();
    order
        .allocate(vec![
            planned("TOKYO", "APPLE", 3),
            planned("TOKYO", "ORANGE", 1),
        ])
        .unwrap();
    assert_eq!(order.shipments().len(), 1);
    assert_eq!(order.shipments()[0].lines().len(), 2);
}

#[test]
fn 明細と数が合わない計画は受け付けない() {
    let mut order = pending_order();
    let result = order.allocate(vec![planned("TOKYO", "APPLE", 3)]); // ORANGE が無い
    assert_eq!(
        result,
        Err(DomainError::PlanMismatch {
            order_id: OrderId::new(1)
        })
    );
    assert_eq!(order.status(), OrderStatus::Pending);
}

#[test]
fn 出荷を一つずつ出すと一部出荷を経て出荷済みになる() {
    let mut order = split_order();

    order.ship(ShipmentNo::new(1), tracking()).unwrap();
    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
    assert_eq!(
        order.shipments()[0].status(),
        &ShipmentStatus::Shipped {
            tracking_number: tracking()
        }
    );

    order.ship(ShipmentNo::new(2), tracking()).unwrap();
    assert_eq!(order.status(), OrderStatus::Shipped);
}

#[test]
fn 同じ出荷を二度は出せない() {
    let mut order = split_order();
    order.ship(ShipmentNo::new(1), tracking()).unwrap();
    let result = order.ship(ShipmentNo::new(1), tracking());
    assert!(matches!(
        result,
        Err(DomainError::ShipmentAlreadyShipped { .. })
    ));
    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
}

#[test]
fn 存在しない出荷番号はエラー() {
    let mut order = split_order();
    let result = order.ship(ShipmentNo::new(9), tracking());
    assert!(matches!(result, Err(DomainError::ShipmentNotFound { .. })));
}

#[test]
fn pending_のまま出荷はできない() {
    let mut order = pending_order();
    let result = order.ship(ShipmentNo::new(1), tracking());
    assert_eq!(
        result,
        Err(DomainError::InvalidTransition {
            order_id: OrderId::new(1),
            from: OrderStatus::Pending,
            action: OrderAction::Ship,
        })
    );
    assert_eq!(order.status(), OrderStatus::Pending);
}

// ---- バックオーダー ----

#[test]
fn 入荷待ちからも引当できる() {
    let mut order = pending_order();
    order.backorder().unwrap();
    assert_eq!(order.status(), OrderStatus::Backordered);

    // 再引当でもまだ足りなければ入荷待ちのまま
    order.backorder().unwrap();
    assert_eq!(order.status(), OrderStatus::Backordered);

    order
        .allocate(vec![
            planned("TOKYO", "APPLE", 3),
            planned("TOKYO", "ORANGE", 1),
        ])
        .unwrap();
    assert_eq!(order.status(), OrderStatus::Allocated);
}

#[test]
fn 引当済みは入荷待ちに戻せない() {
    let mut order = split_order();
    assert!(order.backorder().is_err());
    assert_eq!(order.status(), OrderStatus::Allocated);
}

// ---- キャンセル ----

#[test]
fn pending_と入荷待ちのキャンセルは解放する在庫が無い() {
    let mut pending = pending_order();
    assert!(pending.cancel().unwrap().is_empty());
    assert_eq!(pending.status(), OrderStatus::Cancelled);

    let mut backordered = pending_order();
    backordered.backorder().unwrap();
    assert!(backordered.cancel().unwrap().is_empty());
}

#[test]
fn 引当済みのキャンセルは押さえていた在庫を返し出荷も取り消す() {
    let mut order = split_order();
    let released = order.cancel().unwrap();
    assert_eq!(released.len(), 2);
    assert_eq!(order.status(), OrderStatus::Cancelled);
    assert!(
        order
            .shipments()
            .iter()
            .all(|s| s.status() == &ShipmentStatus::Cancelled)
    );
}

#[test]
fn 一部出荷済みはキャンセルできない() {
    let mut order = split_order();
    order.ship(ShipmentNo::new(1), tracking()).unwrap();
    let result = order.cancel();
    assert!(matches!(
        result,
        Err(DomainError::InvalidTransition {
            from: OrderStatus::PartiallyShipped,
            action: OrderAction::Cancel,
            ..
        })
    ));
    assert_eq!(order.status(), OrderStatus::PartiallyShipped);
}

#[test]
fn 終端の状態からはどこにも遷移できない() {
    let mut shipped = split_order();
    shipped.ship(ShipmentNo::new(1), tracking()).unwrap();
    shipped.ship(ShipmentNo::new(2), tracking()).unwrap();

    let mut cancelled = pending_order();
    cancelled.cancel().unwrap();

    for order in [shipped, cancelled] {
        for action in [
            OrderAction::Allocate,
            OrderAction::Ship,
            OrderAction::Cancel,
        ] {
            assert!(
                order.ensure_can(action).is_err(),
                "{} から {action} できてはいけない",
                order.status()
            );
        }
    }
}
