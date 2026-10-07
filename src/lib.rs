//! 受発注・在庫引当サービスのデモ。
//!
//! バイナリ(main.rs)とは別にライブラリとして切り出しているのは、
//! テストからも各層を直接 `use` できるようにするためと、
//! 「使われていない関数」の警告をバイナリ側の都合で出さないためです。

// `pub mod` は「このモジュールをクレートの外にも公開する」宣言。
// ファイル `src/domain/mod.rs` や `src/service.rs` がそれぞれのモジュール本体になる。
pub mod api;
pub mod domain;
pub mod repository;
pub mod service;
