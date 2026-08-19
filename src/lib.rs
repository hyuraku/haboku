//! haboku のロジック層。**UI フレームワークへの参照をここに持ち込まない。**
//!
//! `vault` / `fuzzy` は gpui 版からそのまま移植したもので、どちらも std のみに依存する。
//! この境界が保たれている限り、UI 層を入れ替えても保存とファジー検索は無傷で動く
//! （gpui 版 → iced 版の移植で、この 2 ファイルは 1 バイトも変えずに済んだ）。
//!
//! バイナリ（`main.rs`）と分けているのは、UI 層から見て「まだ呼んでいない公開 API」が
//! `dead_code` 扱いにならないようにするため。

pub mod config;
pub mod fuzzy;
pub mod testing;
pub mod vault;
