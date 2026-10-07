//! 引当戦略。「どの倉庫から、いくつ出すか」を決める。
//!
//! 倉庫が複数あると、同じ注文でも出し方がいくつもある。
//! どれが良いかは会社の方針(送料、配送日数、倉庫の負荷など)で変わるので、
//! 計画を立てる部分を trait にして差し替えられるようにしている。
//!
//! 戦略は「計画を立てるだけ」で在庫は変えない。
//! 計画どおりに在庫を確保するのは `domain::allocate` の仕事。
//! 計画と実行を分けておくと、戦略のテストが在庫の更新と無関係に書ける。

use super::{OrderLine, Quantity, Sku, Stock, WarehouseId};

/// 計画の 1 行。「この倉庫のこの SKU を、これだけ確保する」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedAllocation {
    pub warehouse: WarehouseId,
    pub sku: Sku,
    pub quantity: Quantity,
}

/// 1 SKU ぶんの在庫不足。数量は全倉庫の合計。
///
/// 不足時は「どれか 1 つ」ではなく「足りない明細すべて」を返す。
/// 利用者が数量を直したり入荷を手配したりするとき、1 つずつ判明する往復を避けるため。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortage {
    pub sku: Sku,
    pub requested: Quantity,
    pub available: Quantity,
}

impl Shortage {
    /// 不足数。保存せず計算で求める(requested / available と食い違わないように)。
    pub fn missing(&self) -> Quantity {
        self.requested.saturating_sub(self.available)
    }
}

/// 引当戦略。
///
/// 全明細を確保できる計画が立てばそれを返し、立たなければ不足を全部返す。
/// 一部だけの計画は返さない(注文単位の all-or-nothing を戦略の側でも守る)。
///
/// `Send + Sync` を要求しているのは、サービスの中に `Box<dyn AllocationStrategy>` として持ち、
/// 複数スレッドから共有するため。(`Sync` は「複数スレッドから同時に参照しても安全」)
pub trait AllocationStrategy: Send + Sync {
    fn plan(
        &self,
        lines: &[OrderLine],
        stocks: &[Stock],
    ) -> Result<Vec<PlannedAllocation>, Vec<Shortage>>;
}

/// 戦略 1: できるだけ 1 つの倉庫から出す。
///
/// 荷物が分かれると送料も受け取る手間も増えるので、まず「1 倉庫で全部そろう」倉庫を
/// 優先順に探す。どこもそろわなければ、優先順に倉庫をまたいで分割する。
pub struct PreferSingleWarehouse {
    priority: Vec<WarehouseId>,
}

impl PreferSingleWarehouse {
    /// `priority` は倉庫の優先順(先頭ほど優先)。
    /// ここに無い倉庫も使うが、優先順位は一番後ろで、倉庫 ID 順になる。
    pub fn new(priority: Vec<WarehouseId>) -> Self {
        PreferSingleWarehouse { priority }
    }
}

impl AllocationStrategy for PreferSingleWarehouse {
    fn plan(
        &self,
        lines: &[OrderLine],
        stocks: &[Stock],
    ) -> Result<Vec<PlannedAllocation>, Vec<Shortage>> {
        check_total_shortages(lines, stocks)?;

        let warehouses = warehouses_by_priority(&self.priority, stocks);
        // 1 倉庫で全明細がそろう、最も優先度の高い倉庫を探す。
        // `iter().all(...)` は「全要素が条件を満たすか」。
        let single = warehouses.iter().find(|warehouse| {
            lines
                .iter()
                .all(|line| available_at(stocks, warehouse, &line.sku) >= line.quantity)
        });

        match single {
            Some(warehouse) => Ok(lines
                .iter()
                .map(|line| PlannedAllocation {
                    warehouse: (*warehouse).clone(),
                    sku: line.sku.clone(),
                    quantity: line.quantity,
                })
                .collect()),
            None => Ok(split_by_priority(lines, stocks, &warehouses)),
        }
    }
}

/// 戦略 2: 明細ごとに、優先度の高い倉庫から順に取れるだけ取る。
///
/// 荷物が分かれやすい代わりに、優先倉庫の在庫を先に使い切れる。
/// (例: 優先倉庫が本社倉庫で、他の倉庫は予備という運用)
pub struct GreedyByPriority {
    priority: Vec<WarehouseId>,
}

impl GreedyByPriority {
    pub fn new(priority: Vec<WarehouseId>) -> Self {
        GreedyByPriority { priority }
    }
}

impl AllocationStrategy for GreedyByPriority {
    fn plan(
        &self,
        lines: &[OrderLine],
        stocks: &[Stock],
    ) -> Result<Vec<PlannedAllocation>, Vec<Shortage>> {
        check_total_shortages(lines, stocks)?;
        let warehouses = warehouses_by_priority(&self.priority, stocks);
        Ok(split_by_priority(lines, stocks, &warehouses))
    }
}

// ---- 戦略で共通に使う部品 ----

/// 全倉庫の合計で足りない明細を、すべて集める。1 つでもあれば Err。
fn check_total_shortages(lines: &[OrderLine], stocks: &[Stock]) -> Result<(), Vec<Shortage>> {
    let shortages: Vec<Shortage> = lines
        .iter()
        // `filter_map` は「Some の要素だけ残して中身を取り出す」map + filter。
        .filter_map(|line| {
            let total = total_available(stocks, &line.sku);
            if total >= line.quantity {
                None
            } else {
                Some(Shortage {
                    sku: line.sku.clone(),
                    requested: line.quantity,
                    available: total,
                })
            }
        })
        .collect();

    if shortages.is_empty() {
        Ok(())
    } else {
        Err(shortages)
    }
}

/// 明細ごとに、倉庫を優先順にたどって取れるだけ取る。
/// 合計で足りることは確認済みである前提。
fn split_by_priority(
    lines: &[OrderLine],
    stocks: &[Stock],
    warehouses: &[&WarehouseId],
) -> Vec<PlannedAllocation> {
    let mut plan = Vec::new();
    for line in lines {
        let mut remaining = line.quantity;
        for warehouse in warehouses {
            if remaining.is_zero() {
                break;
            }
            let take = remaining.min(available_at(stocks, warehouse, &line.sku));
            if !take.is_zero() {
                plan.push(PlannedAllocation {
                    warehouse: (*warehouse).clone(),
                    sku: line.sku.clone(),
                    quantity: take,
                });
                remaining = remaining.saturating_sub(take);
            }
        }
    }
    plan
}

/// 在庫に登場する倉庫を、優先順に重複なく並べる。
///
/// 戻り値の `Vec<&WarehouseId>` は「倉庫 ID への参照」の Vec。
/// 中身は stocks の中の値を借りているだけなので、複製のコストがかからない。
fn warehouses_by_priority<'a>(
    priority: &'a [WarehouseId],
    stocks: &'a [Stock],
) -> Vec<&'a WarehouseId> {
    let mut result: Vec<&WarehouseId> = Vec::new();
    // まず優先リストの順に、在庫がある倉庫だけを入れる
    for warehouse in priority {
        if stocks.iter().any(|s| s.warehouse() == warehouse) && !result.contains(&warehouse) {
            result.push(warehouse);
        }
    }
    // 優先リストに無い倉庫は ID 順で後ろに付ける
    let mut others: Vec<&WarehouseId> = stocks
        .iter()
        .map(Stock::warehouse)
        .filter(|w| !result.contains(w))
        .collect();
    others.sort();
    others.dedup(); // 連続する重複を取り除く(ソート済みなので全重複が消える)
    result.extend(others);
    result
}

fn available_at(stocks: &[Stock], warehouse: &WarehouseId, sku: &Sku) -> Quantity {
    stocks
        .iter()
        .find(|s| s.is(warehouse, sku))
        .map(Stock::available)
        .unwrap_or(Quantity::ZERO) // その倉庫に在庫レコードが無ければ 0 個
}

fn total_available(stocks: &[Stock], sku: &Sku) -> Quantity {
    // `fold` は初期値から始めて要素を 1 つずつ畳み込む。ここでは合計を求めている。
    // 足し算があふれた場合は u32 の上限で止める(引当の判定には十分)。
    stocks
        .iter()
        .filter(|s| s.sku() == sku)
        .fold(Quantity::ZERO, |sum, s| {
            sum.checked_add(s.available())
                .unwrap_or(Quantity::new(u32::MAX))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn line(sku_value: &str, quantity: u32) -> OrderLine {
        OrderLine {
            sku: sku(sku_value),
            quantity: Quantity::new(quantity),
        }
    }

    fn planned(warehouse: &str, sku_value: &str, quantity: u32) -> PlannedAllocation {
        PlannedAllocation {
            warehouse: wh(warehouse),
            sku: sku(sku_value),
            quantity: Quantity::new(quantity),
        }
    }

    fn priority() -> Vec<WarehouseId> {
        vec![wh("TOKYO"), wh("OSAKA")]
    }

    #[test]
    fn 一つの倉庫でそろうなら優先度が低くてもその倉庫から出す() {
        // TOKYO には ORANGE が無いが、OSAKA なら APPLE も ORANGE もそろう
        let stocks = vec![
            stock("TOKYO", "APPLE", 10),
            stock("OSAKA", "APPLE", 5),
            stock("OSAKA", "ORANGE", 5),
        ];
        let plan = PreferSingleWarehouse::new(priority())
            .plan(&[line("APPLE", 3), line("ORANGE", 1)], &stocks)
            .unwrap();
        assert_eq!(
            plan,
            vec![planned("OSAKA", "APPLE", 3), planned("OSAKA", "ORANGE", 1)]
        );
    }

    #[test]
    fn どの倉庫でもそろわなければ優先順に分割する() {
        let stocks = vec![
            stock("TOKYO", "APPLE", 2),
            stock("OSAKA", "APPLE", 5),
            stock("OSAKA", "ORANGE", 1),
        ];
        let plan = PreferSingleWarehouse::new(priority())
            .plan(&[line("APPLE", 6), line("ORANGE", 1)], &stocks)
            .unwrap();
        assert_eq!(
            plan,
            vec![
                planned("TOKYO", "APPLE", 2),
                planned("OSAKA", "APPLE", 4),
                planned("OSAKA", "ORANGE", 1),
            ]
        );
    }

    #[test]
    fn 貪欲戦略は一つの倉庫でそろっても優先倉庫から取る() {
        let stocks = vec![
            stock("TOKYO", "APPLE", 10),
            stock("OSAKA", "APPLE", 5),
            stock("OSAKA", "ORANGE", 5),
        ];
        let plan = GreedyByPriority::new(priority())
            .plan(&[line("APPLE", 3), line("ORANGE", 1)], &stocks)
            .unwrap();
        assert_eq!(
            plan,
            vec![planned("TOKYO", "APPLE", 3), planned("OSAKA", "ORANGE", 1)]
        );
    }

    #[test]
    fn 全倉庫の合計で足りない明細はすべて不足として返る() {
        let stocks = vec![
            stock("TOKYO", "APPLE", 2),
            stock("OSAKA", "APPLE", 2),
            stock("OSAKA", "ORANGE", 1),
            stock("OSAKA", "BANANA", 9),
        ];
        let result = PreferSingleWarehouse::new(priority()).plan(
            &[line("APPLE", 5), line("BANANA", 1), line("ORANGE", 3)],
            &stocks,
        );
        assert_eq!(
            result,
            Err(vec![
                Shortage {
                    sku: sku("APPLE"),
                    requested: Quantity::new(5),
                    available: Quantity::new(4),
                },
                Shortage {
                    sku: sku("ORANGE"),
                    requested: Quantity::new(3),
                    available: Quantity::new(1),
                },
            ])
        );
    }

    #[test]
    fn 優先リストに無い倉庫も後ろの順位で使う() {
        let stocks = vec![stock("TOKYO", "APPLE", 1), stock("FUKUOKA", "APPLE", 5)];
        let plan = GreedyByPriority::new(priority())
            .plan(&[line("APPLE", 3)], &stocks)
            .unwrap();
        assert_eq!(
            plan,
            vec![planned("TOKYO", "APPLE", 1), planned("FUKUOKA", "APPLE", 2)]
        );
    }
}
