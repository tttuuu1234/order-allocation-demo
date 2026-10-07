//! 受注と、その状態遷移。
//!
//! ```text
//!              ┌──────── allocate(不足)───────┐
//!              ▼                               │
//!   Pending ──allocate(不足)──▶ Backordered ──┘
//!     │  │                         │
//!     │  └──allocate(成功)──┐      │ allocate(成功)/ 入荷時の自動再引当
//!     │                     ▼      ▼
//!     │                    Allocated ──ship(一部の出荷)──▶ PartiallyShipped
//!     │                     │   │                              │
//!     │                     │   └──ship(最後の出荷)──▶ Shipped ◀┘ ship(最後の出荷)
//!     ▼                     ▼
//!   Cancelled ◀── cancel ── (Pending / Backordered / Allocated から)
//! ```
//!
//! どの状態からどの操作ができるかは `ensure_can` の 1 か所にまとめている。
//! 遷移表がコードに 1 つだけあれば、ルールの確認も変更もそこを見るだけで済むため。

use std::fmt;

use super::{
    DomainError, OrderId, PlannedAllocation, Quantity, Shipment, ShipmentLine, ShipmentNo, Sku,
    TrackingNumber, WarehouseId,
};

/// 受注の状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    /// 受け付けたが、まだ引当を試していない
    Pending,
    /// 引当を試したが在庫が足りず、入荷待ち(バックオーダー)。在庫は 1 つも押さえていない
    Backordered,
    /// 全明細の在庫を確保済み(出荷待ち)
    Allocated,
    /// 出荷が複数に分かれ、一部だけ出荷済み
    PartiallyShipped,
    /// 全出荷が出荷済み(終端)
    Shipped,
    /// キャンセル済み(終端)
    Cancelled,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` は derive(Debug) の表示。enum ならバリアント名がそのまま出る。
        write!(f, "{self:?}")
    }
}

/// 受注に対する操作。エラーメッセージで「何をしようとして失敗したか」を伝えるのにも使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderAction {
    Allocate,
    Ship,
    Cancel,
}

impl fmt::Display for OrderAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            OrderAction::Allocate => "allocate",
            OrderAction::Ship => "ship",
            OrderAction::Cancel => "cancel",
        };
        write!(f, "{name}")
    }
}

/// 受注明細。1 つの SKU とその数量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderLine {
    pub sku: Sku,
    pub quantity: Quantity,
}

/// 受注。出荷(Shipment)を子として持つ集約。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    id: OrderId,
    lines: Vec<OrderLine>,
    status: OrderStatus,
    /// 引当時に、倉庫ごとに 1 件ずつ作られる
    shipments: Vec<Shipment>,
}

impl Order {
    /// 新しい受注を `Pending` で作る。
    ///
    /// - 明細が空ならエラー
    /// - 数量 0 の明細があればエラー
    /// - 同じ SKU の明細は 1 行に合算する
    ///
    /// 合算するのは、同じ SKU が 2 行あると引当時に
    /// 「1 行目は確保できたが 2 行目で足りない」という判定が複雑になるため。
    ///
    /// 引数の `lines: Vec<OrderLine>` は参照(`&`)ではなく値で受け取っている。
    /// これは「所有権を受け取る」という意味で、呼び出し元はこの後 `lines` を使えなくなる(ムーブ)。
    /// 中身を作り直して Order に持たせるので、借りるより受け取る方が素直。
    pub fn new(id: OrderId, lines: Vec<OrderLine>) -> Result<Self, DomainError> {
        if lines.is_empty() {
            return Err(DomainError::EmptyOrderLines);
        }

        // HashMap ではなく Vec で合算しているのは、利用者が送った明細の順序を保つため。
        // 明細数は多くても数十程度の想定なので、線形探索でも問題にならない。
        let mut merged: Vec<OrderLine> = Vec::new();
        // `for line in lines` で Vec の所有権ごと 1 つずつ取り出す(`lines` はここで消費される)。
        for line in lines {
            if line.quantity.is_zero() {
                return Err(DomainError::ZeroQuantity { sku: line.sku });
            }
            // `iter_mut().find(...)` は条件に合う最初の要素への可変参照を Option で返す。
            match merged.iter_mut().find(|existing| existing.sku == line.sku) {
                Some(existing) => {
                    existing.quantity =
                        existing
                            .quantity
                            .checked_add(line.quantity)
                            .ok_or_else(|| DomainError::QuantityOverflow {
                                sku: line.sku.clone(),
                            })?;
                }
                None => merged.push(line),
            }
        }

        Ok(Order {
            id,
            lines: merged,
            status: OrderStatus::Pending,
            shipments: Vec::new(),
        })
    }

    pub fn id(&self) -> OrderId {
        self.id
    }

    /// 明細をスライス(`&[T]`)で返す。
    /// `&Vec<T>` より `&[T]` の方が「読むだけ」の意図が明確で、内部表現にも縛られない。
    pub fn lines(&self) -> &[OrderLine] {
        &self.lines
    }

    pub fn status(&self) -> OrderStatus {
        self.status
    }

    pub fn shipments(&self) -> &[Shipment] {
        &self.shipments
    }

    pub fn contains_sku(&self, sku: &Sku) -> bool {
        self.lines.iter().any(|line| &line.sku == sku)
    }

    /// 状態遷移表。現在の状態でその操作が許されるかを判定する。
    ///
    /// 遷移先は操作ごとのメソッドが決める(出荷は「残りの出荷があるか」で行き先が変わるため)。
    /// ここは「許されるか」だけに集中させて、表として読めるようにしている。
    pub fn ensure_can(&self, action: OrderAction) -> Result<(), DomainError> {
        use OrderStatus::*; // この関数の中だけ `OrderStatus::` を省略して書けるようにする

        // `matches!(値, パターン)` は値がパターンに一致するかを bool で返す。
        // `|` は「または」。
        let allowed = match action {
            OrderAction::Allocate => matches!(self.status, Pending | Backordered),
            OrderAction::Ship => matches!(self.status, Allocated | PartiallyShipped),
            // 一部でも出荷したら取り消せない。物はもう倉庫を出ているので、
            // 実務では「キャンセル」ではなく「返品」という別の業務になる。
            OrderAction::Cancel => matches!(self.status, Pending | Backordered | Allocated),
        };
        if allowed {
            Ok(())
        } else {
            Err(DomainError::InvalidTransition {
                order_id: self.id,
                from: self.status,
                action,
            })
        }
    }

    /// 引当計画を受け取り、倉庫ごとの出荷を作って Allocated にする。
    /// 在庫の確保は `domain::allocate` が担う。
    pub fn allocate(&mut self, plan: Vec<PlannedAllocation>) -> Result<(), DomainError> {
        self.ensure_can(OrderAction::Allocate)?;
        self.ensure_plan_covers_lines(&plan)?;

        // 倉庫ごとにまとめる。倉庫の並びは計画に最初に出てきた順(= 戦略の優先順)。
        let mut groups: Vec<(WarehouseId, Vec<ShipmentLine>)> = Vec::new();
        for item in plan {
            let line = ShipmentLine {
                sku: item.sku,
                quantity: item.quantity,
            };
            // `position` は条件に合う最初の要素の添字を Option で返す。
            match groups.iter().position(|(w, _)| *w == item.warehouse) {
                Some(index) => groups[index].1.push(line),
                None => groups.push((item.warehouse, vec![line])),
            }
        }

        // `enumerate` は (添字, 要素) の組を順に返す。出荷番号は 1 から振る。
        self.shipments = groups
            .into_iter()
            .enumerate()
            .map(|(index, (warehouse, lines))| {
                Shipment::new(ShipmentNo::new(index as u32 + 1), warehouse, lines)
            })
            .collect();
        self.status = OrderStatus::Allocated;
        Ok(())
    }

    /// 入荷待ちにする。在庫は何も押さえない。
    pub fn backorder(&mut self) -> Result<(), DomainError> {
        // 引当を試した結果として入るので、引当と同じ遷移条件を使う。
        self.ensure_can(OrderAction::Allocate)?;
        self.status = OrderStatus::Backordered;
        Ok(())
    }

    /// 出荷を 1 件、出荷済みにする。在庫の減算は `domain::ship` が担う。
    ///
    /// 戻り値は出荷済みにした出荷の複製(在庫をどこからいくつ減らすかに使う)。
    pub fn ship(
        &mut self,
        no: ShipmentNo,
        tracking_number: TrackingNumber,
    ) -> Result<Shipment, DomainError> {
        self.ensure_can(OrderAction::Ship)?;

        let order_id = self.id;
        let shipment = self.shipments.iter_mut().find(|s| s.no() == no).ok_or(
            DomainError::ShipmentNotFound {
                order_id,
                shipment_no: no,
            },
        )?;
        if !shipment.is_awaiting() {
            return Err(DomainError::ShipmentAlreadyShipped {
                order_id,
                shipment_no: no,
            });
        }
        shipment.mark_shipped(tracking_number);
        let shipped = shipment.clone();

        // 全部の出荷が終わったかで、注文の行き先が変わる。
        let all_shipped = self.shipments.iter().all(|s| !s.is_awaiting());
        self.status = if all_shipped {
            OrderStatus::Shipped
        } else {
            OrderStatus::PartiallyShipped
        };
        Ok(shipped)
    }

    /// キャンセルする。引当の解放は `domain::cancel` が担う。
    ///
    /// 戻り値は、この注文が押さえていた在庫(倉庫, 明細)の一覧。
    /// `(A, B)` はタプルで、名前の無い組を手軽に返したいときに使う。
    pub fn cancel(&mut self) -> Result<Vec<(WarehouseId, ShipmentLine)>, DomainError> {
        self.ensure_can(OrderAction::Cancel)?;

        let mut released = Vec::new();
        for shipment in &mut self.shipments {
            if shipment.is_awaiting() {
                for line in shipment.lines() {
                    released.push((shipment.warehouse().clone(), line.clone()));
                }
                shipment.mark_cancelled();
            }
        }
        self.status = OrderStatus::Cancelled;
        Ok(released)
    }

    /// 計画の数量が、SKU ごとに明細の数量とぴったり一致するか。
    ///
    /// 戦略が正しければ必ず一致する。一致しなければ戦略のバグなので、
    /// 少なすぎる・多すぎる引当を黙って受け入れないよう、ここで止める。
    fn ensure_plan_covers_lines(&self, plan: &[PlannedAllocation]) -> Result<(), DomainError> {
        let planned_total = |sku: &Sku| {
            plan.iter()
                .filter(|p| &p.sku == sku)
                .map(|p| u64::from(p.quantity.value()))
                .sum::<u64>()
        };
        let covers_every_line = self
            .lines
            .iter()
            .all(|line| planned_total(&line.sku) == u64::from(line.quantity.value()));
        let has_extra_sku = plan.iter().any(|p| !self.contains_sku(&p.sku));

        if covers_every_line && !has_extra_sku {
            Ok(())
        } else {
            Err(DomainError::PlanMismatch { order_id: self.id })
        }
    }
}

#[cfg(test)]
mod tests;
