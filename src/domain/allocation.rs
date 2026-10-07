//! 受注と在庫をまたぐ操作(引当・出荷・キャンセル・入荷待ちの再引当)。
//!
//! Order と Stock のどちらか一方のメソッドにすると、
//! もう一方の内部を知る必要が出てくるので、両者を受け取る関数として独立させている。
//! (DDD でいう「ドメインサービス」にあたる)
//!
//! どの関数も「コピーで計算 → 全部成功したら最後に書き戻す」形で書いている。
//! 途中で失敗しても、引数の order と stocks には何も反映されない。
//! サービス層もコピーに対して操作するので二重の守りになるが、
//! ドメイン単体で正しいことをテストで確かめられるようにしておきたいため。

use super::{
    AllocationStrategy, DomainError, MovementReason, Order, OrderAction, OrderId, OrderStatus,
    ShipmentNo, Shortage, Sku, Stock, StockMovement, TrackingNumber, WarehouseId,
};

/// 引当の結果。
///
/// 在庫不足はエラーではなく「入荷待ちになった」という正常な結果として返す。
/// 実務では在庫切れは日常的に起きることで、注文を断るのではなく待たせるのが普通だから。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllocationOutcome {
    Allocated,
    Backordered { shortages: Vec<Shortage> },
}

/// 引当。全明細を確保できるときだけ確保し、1 つでも足りなければ在庫には一切触れず
/// 入荷待ち(Backordered)にする(注文単位の all-or-nothing)。
///
/// `stocks` には注文の全 SKU について、全倉庫の在庫が含まれている前提。
/// `strategy: &dyn AllocationStrategy` は「AllocationStrategy を実装した何か」への参照。
/// 具体的な型は実行時に決まる(動的ディスパッチ。Swift の `any Protocol` にあたる)。
pub fn allocate(
    order: &mut Order,
    stocks: &mut [Stock],
    strategy: &dyn AllocationStrategy,
) -> Result<AllocationOutcome, DomainError> {
    // 状態遷移できるかを先に確認する。
    // 在庫不足より先に調べるのは、出荷済みの注文に「在庫不足」と返すと誤解を招くため。
    order.ensure_can(OrderAction::Allocate)?;

    let plan = match strategy.plan(order.lines(), stocks) {
        Ok(plan) => plan,
        Err(shortages) => {
            order.backorder()?;
            return Ok(AllocationOutcome::Backordered { shortages });
        }
    };

    let mut updated_order = order.clone();
    let mut updated_stocks = stocks.to_vec();
    for item in &plan {
        find_stock_mut(&mut updated_stocks, &item.warehouse, &item.sku)?.reserve(item.quantity)?;
    }
    updated_order.allocate(plan)?;

    // ここまで来たら全部成功。まとめて書き戻す。
    // `*order = ...` は参照の先にある値そのものを置き換える(`*` は参照外し)。
    *order = updated_order;
    // `clone_from_slice` は長さが同じスライスへ中身をまとめて複製する。
    stocks.clone_from_slice(&updated_stocks);
    Ok(AllocationOutcome::Allocated)
}

/// 出荷を 1 件出す。その出荷の倉庫の在庫から、実在庫と引当済みの両方を減らす。
///
/// 戻り値は受払台帳に記録する在庫の動き。
pub fn ship(
    order: &mut Order,
    shipment_no: ShipmentNo,
    tracking_number: TrackingNumber,
    stocks: &mut [Stock],
) -> Result<Vec<StockMovement>, DomainError> {
    let mut updated_order = order.clone();
    let shipment = updated_order.ship(shipment_no, tracking_number)?;

    let mut updated_stocks = stocks.to_vec();
    let mut movements = Vec::new();
    for line in shipment.lines() {
        let reason = MovementReason::Shipment {
            order_id: order.id(),
            shipment_no,
        };
        let stock = find_stock_mut(&mut updated_stocks, shipment.warehouse(), &line.sku)?;
        movements.push(stock.ship(line.quantity, reason)?);
    }

    *order = updated_order;
    stocks.clone_from_slice(&updated_stocks);
    Ok(movements)
}

/// キャンセル。まだ出荷していない出荷が押さえていた在庫を解放する。
pub fn cancel(order: &mut Order, stocks: &mut [Stock]) -> Result<(), DomainError> {
    let mut updated_order = order.clone();
    // Pending / Backordered なら解放するものは無く、空の一覧が返る。
    let released = updated_order.cancel()?;

    let mut updated_stocks = stocks.to_vec();
    for (warehouse, line) in &released {
        find_stock_mut(&mut updated_stocks, warehouse, &line.sku)?.release(line.quantity)?;
    }

    *order = updated_order;
    stocks.clone_from_slice(&updated_stocks);
    Ok(())
}

/// 入荷待ちの注文を、渡された順に引き当て直す。引当できた注文の ID を返す。
///
/// 入荷・棚卸での増加・キャンセルで「引当可能数が増えた」ときに呼ぶ。
/// 渡す順番がそのまま優先順位になる(サービス層では注文 ID 順 = 先着順で渡している)。
///
/// 1 件ずつ all-or-nothing で試すので、先の注文が足りずに待ちのままでも、
/// 後ろの小さな注文は引当できることがある(追い越し。README のトレードオフ参照)。
pub fn reallocate_backorders(
    orders: &mut [Order],
    stocks: &mut [Stock],
    strategy: &dyn AllocationStrategy,
) -> Result<Vec<OrderId>, DomainError> {
    let mut allocated = Vec::new();
    for order in orders.iter_mut() {
        if order.status() != OrderStatus::Backordered {
            continue;
        }
        // 1 件ごとに stocks が更新されるので、次の注文は前の注文が確保した後の在庫で判定される。
        if allocate(order, stocks, strategy)? == AllocationOutcome::Allocated {
            allocated.push(order.id());
        }
    }
    Ok(allocated)
}

fn find_stock_mut<'a>(
    stocks: &'a mut [Stock],
    warehouse: &WarehouseId,
    sku: &Sku,
) -> Result<&'a mut Stock, DomainError> {
    // `'a` はライフタイム注釈。「戻り値の参照は引数 stocks と同じだけ生きる」ことを
    // コンパイラに伝えている。参照を返す関数で、元データが先に消える事故を防ぐ仕組み。
    stocks
        .iter_mut()
        .find(|stock| stock.is(warehouse, sku))
        .ok_or_else(|| DomainError::StockMissing {
            warehouse: warehouse.clone(),
            sku: sku.clone(),
        })
}

#[cfg(test)]
mod tests;
