//! 受注と在庫をまたぐ操作(引当・出荷・キャンセル)。
//!
//! Order と Stock のどちらか一方のメソッドにすると、
//! もう一方の内部を知る必要が出てくるので、両者を受け取る関数として独立させている。
//! (DDD でいう「ドメインサービス」にあたる)
//!
//! どの関数も「失敗したら order も stocks も一切変えない」ことを保証する。
//! サービス層もコピーに対して操作するので二重の守りになるが、
//! ドメイン単体で正しいことをテストで確かめられるようにしておきたいため。

use super::{DomainError, Order, OrderAction, OrderStatus, Shortage, Sku, Stock};

/// 引当。全明細を確保できるときだけ確保し、1 つでも足りなければ何も変えない(all-or-nothing)。
///
/// `stocks` には注文の全 SKU の在庫が含まれている前提。
pub fn allocate(order: &mut Order, stocks: &mut [Stock]) -> Result<(), DomainError> {
    // 1. 状態遷移できるかを先に確認する。
    //    在庫不足より先に調べるのは、出荷済みの注文に「在庫不足」と返すと誤解を招くため。
    order.next_status(OrderAction::Allocate)?;

    // 2. まだ何も変えずに、全明細の不足を集める。
    //    途中で return せず最後まで調べるのは、不足を一括で返すという業務ルールのため。
    let mut shortages: Vec<Shortage> = Vec::new();
    for line in order.lines() {
        let stock = find_stock(stocks, &line.sku)?;
        if let Some(shortage) = stock.shortage_for(line.quantity) {
            shortages.push(shortage);
        }
    }
    if !shortages.is_empty() {
        return Err(DomainError::InsufficientStock { shortages });
    }

    // 3. ここに来たら全明細が確保できることが分かっているので、実際に変更する。
    //    明細は SKU ごとに合算済み(Order::new)なので、同じ在庫を二重に引き当てることはない。
    //    理屈の上ではもう失敗しないが、ship / cancel と同じく
    //    「コピーで計算 → 最後に書き戻す」形にして、万一の途中失敗でも引数を汚さない。
    let mut updated: Vec<Stock> = stocks.to_vec();
    for line in order.lines() {
        find_stock_mut(&mut updated, &line.sku)?.reserve(line.quantity)?;
    }
    stocks.clone_from_slice(&updated);
    order.allocate()
}

/// 出荷。引当済みの数量を、実在庫と引当済みの両方から減らす。
pub fn ship(order: &mut Order, stocks: &mut [Stock]) -> Result<(), DomainError> {
    order.next_status(OrderAction::Ship)?;

    // 各在庫の変更は「コピー(`to_vec`)で計算 → 最後にまとめて書き戻す」形にする。
    // 途中の明細で失敗しても、引数の stocks には何も反映されていない状態を保つため。
    let mut updated: Vec<Stock> = stocks.to_vec();
    for line in order.lines() {
        find_stock_mut(&mut updated, &line.sku)?.ship(line.quantity)?;
    }

    // `clone_from_slice` は長さが同じスライスへ中身をまとめて複製する。
    stocks.clone_from_slice(&updated);
    order.ship()
}

/// キャンセル。引当済みなら引当を解放する。
pub fn cancel(order: &mut Order, stocks: &mut [Stock]) -> Result<(), DomainError> {
    order.next_status(OrderAction::Cancel)?;

    // Pending のキャンセルは在庫に触れない。まだ何も確保していないから。
    // 状態を見て分岐するのはここだけにし、Order 側には在庫の知識を持たせない。
    let was_allocated = order.status() == OrderStatus::Allocated;
    if was_allocated {
        let mut updated: Vec<Stock> = stocks.to_vec();
        for line in order.lines() {
            find_stock_mut(&mut updated, &line.sku)?.release(line.quantity)?;
        }
        stocks.clone_from_slice(&updated);
    }
    order.cancel()
}

fn find_stock<'a>(stocks: &'a [Stock], sku: &Sku) -> Result<&'a Stock, DomainError> {
    // `'a` はライフタイム注釈。「戻り値の参照は引数 stocks と同じだけ生きる」ことを
    // コンパイラに伝えている。参照を返す関数で、元データが先に消える事故を防ぐ仕組み。
    stocks
        .iter()
        .find(|stock| stock.sku() == sku)
        .ok_or_else(|| DomainError::StockMissing { sku: sku.clone() })
}

fn find_stock_mut<'a>(stocks: &'a mut [Stock], sku: &Sku) -> Result<&'a mut Stock, DomainError> {
    stocks
        .iter_mut()
        .find(|stock| stock.sku() == sku)
        .ok_or_else(|| DomainError::StockMissing { sku: sku.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{OrderId, OrderLine, Quantity};

    fn sku(value: &str) -> Sku {
        Sku::new(value).unwrap()
    }

    fn stock(value: &str, on_hand: u32) -> Stock {
        let mut stock = Stock::new(sku(value));
        stock.receive(Quantity::new(on_hand)).unwrap();
        stock
    }

    fn order(lines: &[(&str, u32)]) -> Order {
        let lines = lines
            .iter()
            .map(|(s, q)| OrderLine {
                sku: sku(s),
                quantity: Quantity::new(*q),
            })
            .collect();
        Order::new(OrderId::new(1), lines).unwrap()
    }

    #[test]
    fn 全明細が確保できれば引当される() {
        let mut order = order(&[("APPLE", 3), ("BANANA", 2)]);
        let mut stocks = vec![stock("APPLE", 10), stock("BANANA", 2)];

        allocate(&mut order, &mut stocks).unwrap();

        assert_eq!(order.status(), OrderStatus::Allocated);
        assert_eq!(stocks[0].reserved(), Quantity::new(3));
        assert_eq!(stocks[1].reserved(), Quantity::new(2));
        assert_eq!(stocks[1].available(), Quantity::ZERO);
    }

    #[test]
    fn 一部でも不足すれば何も変わらず_不足は全明細ぶん返る() {
        let mut order = order(&[("APPLE", 3), ("BANANA", 5), ("CHERRY", 9)]);
        let mut stocks = vec![stock("APPLE", 10), stock("BANANA", 2), stock("CHERRY", 1)];
        let order_before = order.clone();
        let stocks_before = stocks.clone();

        let result = allocate(&mut order, &mut stocks);

        // APPLE は足りているので含まれず、BANANA と CHERRY の不足が両方返る
        assert_eq!(
            result,
            Err(DomainError::InsufficientStock {
                shortages: vec![
                    Shortage {
                        sku: sku("BANANA"),
                        requested: Quantity::new(5),
                        available: Quantity::new(2),
                    },
                    Shortage {
                        sku: sku("CHERRY"),
                        requested: Quantity::new(9),
                        available: Quantity::new(1),
                    },
                ]
            })
        );
        // 足りていた APPLE も含め、在庫も注文も一切変わっていない
        assert_eq!(order, order_before);
        assert_eq!(stocks, stocks_before);
    }

    #[test]
    fn 引当済みの注文を再度引き当てると状態エラーで在庫は変わらない() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];
        allocate(&mut order, &mut stocks).unwrap();
        let stocks_before = stocks.clone();

        let result = allocate(&mut order, &mut stocks);

        assert!(matches!(result, Err(DomainError::InvalidTransition { .. })));
        assert_eq!(stocks, stocks_before);
    }

    #[test]
    fn 出荷で実在庫と引当済みが減る() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];
        allocate(&mut order, &mut stocks).unwrap();

        ship(&mut order, &mut stocks).unwrap();

        assert_eq!(order.status(), OrderStatus::Shipped);
        assert_eq!(stocks[0].on_hand(), Quantity::new(7));
        assert_eq!(stocks[0].reserved(), Quantity::ZERO);
    }

    #[test]
    fn 引当前の出荷は状態エラーで在庫は変わらない() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];

        let result = ship(&mut order, &mut stocks);

        assert!(matches!(
            result,
            Err(DomainError::InvalidTransition {
                from: OrderStatus::Pending,
                ..
            })
        ));
        assert_eq!(stocks[0].on_hand(), Quantity::new(10));
    }

    #[test]
    fn 引当済みのキャンセルは引当を解放する() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];
        allocate(&mut order, &mut stocks).unwrap();

        cancel(&mut order, &mut stocks).unwrap();

        assert_eq!(order.status(), OrderStatus::Cancelled);
        assert_eq!(stocks[0].reserved(), Quantity::ZERO);
        assert_eq!(stocks[0].available(), Quantity::new(10));
    }

    #[test]
    fn 未引当のキャンセルは在庫に触れない() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];
        let stocks_before = stocks.clone();

        cancel(&mut order, &mut stocks).unwrap();

        assert_eq!(order.status(), OrderStatus::Cancelled);
        assert_eq!(stocks, stocks_before);
    }

    #[test]
    fn 出荷済みはキャンセルできず在庫も変わらない() {
        let mut order = order(&[("APPLE", 3)]);
        let mut stocks = vec![stock("APPLE", 10)];
        allocate(&mut order, &mut stocks).unwrap();
        ship(&mut order, &mut stocks).unwrap();
        let stocks_before = stocks.clone();

        let result = cancel(&mut order, &mut stocks);

        assert!(matches!(
            result,
            Err(DomainError::InvalidTransition {
                from: OrderStatus::Shipped,
                ..
            })
        ));
        assert_eq!(order.status(), OrderStatus::Shipped);
        assert_eq!(stocks, stocks_before);
    }
}
