//! 在庫。
//!
//! 不変条件: `reserved <= on_hand`。
//! つまり `available = on_hand - reserved` は決してマイナスにならない。
//! この条件を守るため、フィールドは非公開にしてメソッド経由でしか変更できないようにする。

use super::{DomainError, Quantity, Shortage, Sku};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stock {
    sku: Sku,
    /// 実在庫(倉庫に物理的にある数)
    on_hand: Quantity,
    /// 引当済み(注文に確保されているが、まだ出荷していない数)
    reserved: Quantity,
}

impl Stock {
    /// 在庫 0 の新しい在庫レコードを作る。入荷時に SKU が未登録なら使う。
    pub fn new(sku: Sku) -> Self {
        Stock {
            sku,
            on_hand: Quantity::ZERO,
            reserved: Quantity::ZERO,
        }
    }

    // ---- 読み取り ----
    // `&self` は「自分を読み取り専用で借りる」という意味。
    // 呼び出し後も呼び出し元が Stock を使い続けられる。

    pub fn sku(&self) -> &Sku {
        &self.sku
    }

    pub fn on_hand(&self) -> Quantity {
        self.on_hand
    }

    pub fn reserved(&self) -> Quantity {
        self.reserved
    }

    /// 引当可能数。保存せず毎回計算するのは、on_hand / reserved と食い違う余地をなくすため。
    pub fn available(&self) -> Quantity {
        // 不変条件 reserved <= on_hand があるので負にならないが、
        // 万一に備えて saturating_sub(0 で止まる減算)にしておく。
        self.on_hand.saturating_sub(self.reserved)
    }

    /// この在庫で `requested` を引当できないなら、その不足を返す。
    ///
    /// 状態を変えずに「できるかどうか」だけを調べる。
    /// all-or-nothing の引当では、全明細を先にこれで調べてから実際に変更する。
    pub fn shortage_for(&self, requested: Quantity) -> Option<Shortage> {
        if self.available() >= requested {
            None
        } else {
            Some(Shortage {
                sku: self.sku.clone(),
                requested,
                available: self.available(),
            })
        }
    }

    // ---- 変更 ----
    // `&mut self` は「自分を書き換え可能として借りる」。
    // Rust では同時に 1 か所しか `&mut` で借りられないので、データ競合がコンパイル時に防がれる。

    /// 入荷。実在庫を増やす。
    pub fn receive(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        if quantity.is_zero() {
            return Err(DomainError::ZeroQuantity {
                sku: self.sku.clone(),
            });
        }
        // `ok_or_else` は Option を Result に変換する。None ならクロージャでエラーを作る。
        // 末尾の `?` は「Err ならその場で return し、Ok なら中身を取り出す」演算子。
        // Swift の `try` に近いが、関数の戻り値も Result である必要がある。
        self.on_hand =
            self.on_hand
                .checked_add(quantity)
                .ok_or_else(|| DomainError::QuantityOverflow {
                    sku: self.sku.clone(),
                })?;
        Ok(())
    }

    /// 引当。引当可能数が足りなければ何も変えずにエラー。
    pub fn reserve(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        if let Some(shortage) = self.shortage_for(quantity) {
            return Err(DomainError::InsufficientStock {
                shortages: vec![shortage],
            });
        }
        // shortage_for で available >= quantity を確認済みなので、
        // reserved + quantity <= on_hand <= u32::MAX となり、あふれない。
        self.reserved =
            self.reserved
                .checked_add(quantity)
                .ok_or_else(|| DomainError::QuantityOverflow {
                    sku: self.sku.clone(),
                })?;
        Ok(())
    }

    /// 引当の解放(キャンセル時)。
    pub fn release(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        self.reserved = self.subtract_reserved(quantity)?;
        Ok(())
    }

    /// 出荷。倉庫から物が出ていくので、実在庫と引当済みの両方を減らす。
    /// 両方減らすので available は変わらない。
    pub fn ship(&mut self, quantity: Quantity) -> Result<(), DomainError> {
        // 先に両方の計算を済ませてから代入する。
        // 片方だけ代入した後にもう片方で失敗すると、中途半端な状態が残るため。
        let new_reserved = self.subtract_reserved(quantity)?;
        let new_on_hand =
            self.on_hand
                .checked_sub(quantity)
                .ok_or_else(|| DomainError::ReservedUnderflow {
                    sku: self.sku.clone(),
                })?;
        self.reserved = new_reserved;
        self.on_hand = new_on_hand;
        Ok(())
    }

    fn subtract_reserved(&self, quantity: Quantity) -> Result<Quantity, DomainError> {
        self.reserved
            .checked_sub(quantity)
            .ok_or_else(|| DomainError::ReservedUnderflow {
                sku: self.sku.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock(on_hand: u32, reserved: u32) -> Stock {
        let mut stock = Stock::new(Sku::new("APPLE").unwrap());
        if on_hand > 0 {
            stock.receive(Quantity::new(on_hand)).unwrap();
        }
        if reserved > 0 {
            stock.reserve(Quantity::new(reserved)).unwrap();
        }
        stock
    }

    #[test]
    fn available_は実在庫から引当済みを引いた数() {
        let stock = stock(10, 3);
        assert_eq!(stock.available(), Quantity::new(7));
    }

    #[test]
    fn 入荷で実在庫が増える() {
        let mut stock = stock(10, 0);
        stock.receive(Quantity::new(5)).unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(15));
    }

    #[test]
    fn 数量0の入荷はエラー() {
        let mut stock = stock(10, 0);
        let result = stock.receive(Quantity::ZERO);
        // `matches!` はパターンに一致するかを bool で返すマクロ。中身の値までは問わないときに便利。
        assert!(matches!(result, Err(DomainError::ZeroQuantity { .. })));
    }

    #[test]
    fn 引当可能数を超える引当は失敗し在庫は変わらない() {
        let mut stock = stock(5, 3);
        let before = stock.clone();

        let result = stock.reserve(Quantity::new(3));

        assert_eq!(
            result,
            Err(DomainError::InsufficientStock {
                shortages: vec![Shortage {
                    sku: Sku::new("APPLE").unwrap(),
                    requested: Quantity::new(3),
                    available: Quantity::new(2),
                }]
            })
        );
        assert_eq!(stock, before);
    }

    #[test]
    fn 出荷で実在庫と引当済みが両方減る() {
        let mut stock = stock(10, 4);
        stock.ship(Quantity::new(4)).unwrap();
        assert_eq!(stock.on_hand(), Quantity::new(6));
        assert_eq!(stock.reserved(), Quantity::ZERO);
        assert_eq!(stock.available(), Quantity::new(6));
    }

    #[test]
    fn 引当済みを超える解放は内部不整合エラー() {
        let mut stock = stock(10, 2);
        let result = stock.release(Quantity::new(3));
        assert!(matches!(result, Err(DomainError::ReservedUnderflow { .. })));
        assert_eq!(stock.reserved(), Quantity::new(2));
    }
}
