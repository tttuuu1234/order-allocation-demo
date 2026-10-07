//! 値オブジェクト(newtype)。
//!
//! `String` や `u32` をそのまま使わず専用の型で包むのは、
//! 「SKU を渡すべき引数に注文 ID を渡してしまう」ような取り違えを
//! コンパイラに検出させるため。Swift の `struct Sku { let value: String }` や
//! Kotlin の `@JvmInline value class Sku(val value: String)` と同じ発想。

use std::fmt;

use super::DomainError;

/// 商品の識別子。空文字は認めない。
///
/// `#[derive(...)]` は、比較やコピーなどの定型実装をコンパイラに自動生成させる指定。
/// - `Clone`: `.clone()` で明示的に複製できる(Rust は暗黙コピーをしないため必要)
/// - `PartialEq, Eq`: `==` で比較できる
/// - `PartialOrd, Ord`: 大小比較できる(BTreeMap のキーにするため)
/// - `Hash`: HashMap のキーにできる
/// - `Debug`: `{:?}` でデバッグ表示できる(テストの assert 失敗時に中身が見える)
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sku(String); // フィールドを非公開にし、`Sku::new` を通さないと作れないようにする

impl Sku {
    /// 検証付きのコンストラクタ。
    ///
    /// 戻り値の `Result<T, E>` は「成功なら `Ok(T)`、失敗なら `Err(E)`」を表す型。
    /// Swift の `throws` / Kotlin の `Result` に近いが、エラーの型がシグネチャに現れ、
    /// 呼び出し側が処理を書かないと警告になる点が異なる。
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        // `impl Into<String>` は「String に変換できる何か」を受け取る書き方。
        // これで `Sku::new("APPLE")`(&str)も `Sku::new(some_string)`(String)も書ける。
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(DomainError::EmptySku);
        }
        Ok(Sku(trimmed.to_string()))
    }

    /// 中身の文字列を借用で返す。
    ///
    /// `&str` を返すのは、呼び出し側が読むだけなら複製(アロケーション)が不要だから。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// `Display` を実装すると `format!("{}", sku)` や `sku.to_string()` が使えるようになる。
// エラーメッセージに SKU を埋め込むときに使う。
impl fmt::Display for Sku {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 倉庫の識別子(例: "TOKYO")。
///
/// Sku と同じく String の newtype。中身が同じ String でも型が違うので、
/// `Stock::new(sku, warehouse)` のように引数の順番を間違えるとコンパイルエラーになる。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WarehouseId(String);

impl WarehouseId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(DomainError::EmptyWarehouseId);
        }
        Ok(WarehouseId(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WarehouseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 数量。在庫数・注文数の両方に使う。
///
/// `u32` を選んだのは、負の在庫を型レベルで表現できなくするため。
/// 減算でマイナスになりうる箇所は `checked_sub` で明示的に扱う。
///
/// `Copy` を付けているのは、数値のように小さく、複製が安価な値だから。
/// `Copy` 型は代入や関数呼び出しで「ムーブ(所有権の移動)」ではなく自動でコピーされる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Quantity(u32);

impl Quantity {
    pub const ZERO: Quantity = Quantity(0);

    /// 0 も許す。在庫が 0 なのは正常な状態だから。
    /// 「0 を許さない」のは注文明細や入荷などの文脈ごとのルールなので、そちらで検証する。
    pub fn new(value: u32) -> Self {
        Quantity(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// 加算。あふれた場合は `None`。
    ///
    /// `Option<T>` は「値があるなら `Some(T)`、無いなら `None`」を表す型で、
    /// Swift の `T?` / Kotlin の `T?` にあたる。Rust には null が無いので、
    /// 「無いかもしれない」は必ずこの型で表す。
    pub fn checked_add(self, other: Quantity) -> Option<Quantity> {
        // `u32::checked_add` も Option を返すので、`.map` で中身だけ Quantity に包み直す。
        self.0.checked_add(other.0).map(Quantity)
    }

    /// 減算。マイナスになる場合は `None`。
    pub fn checked_sub(self, other: Quantity) -> Option<Quantity> {
        self.0.checked_sub(other.0).map(Quantity)
    }

    /// 減算。マイナスになる場合は 0 に丸める。
    ///
    /// 不足数の計算(「あと何個足りないか」)のように、
    /// 0 未満に意味が無い場面でだけ使う。
    pub fn saturating_sub(self, other: Quantity) -> Quantity {
        Quantity(self.0.saturating_sub(other.0))
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 注文 ID。連番で採番する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderId(u64);

impl OrderId {
    pub fn new(value: u64) -> Self {
        OrderId(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// `#[cfg(test)]` を付けたモジュールは `cargo test` のときだけコンパイルされる。
// テストを同じファイルに置けるので、非公開の関数もテストしやすい。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sku_は前後の空白を取り除く() {
        let sku = Sku::new("  APPLE ").unwrap();
        assert_eq!(sku.as_str(), "APPLE");
    }

    #[test]
    fn 空白だけの_sku_はエラー() {
        assert_eq!(Sku::new("   "), Err(DomainError::EmptySku));
    }

    #[test]
    fn quantity_の減算はマイナスにならない() {
        assert_eq!(Quantity::new(1).checked_sub(Quantity::new(2)), None);
        assert_eq!(
            Quantity::new(1).saturating_sub(Quantity::new(2)),
            Quantity::ZERO
        );
    }

    #[test]
    fn quantity_の加算はあふれを検出する() {
        assert_eq!(Quantity::new(u32::MAX).checked_add(Quantity::new(1)), None);
    }
}
