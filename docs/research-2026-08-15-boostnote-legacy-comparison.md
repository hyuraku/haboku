# 調査: Boostnote-legacy と haboku の仕様比較（2026-08-15）

<!-- 今後の機能取捨選択（何を意図的に落とし、何を引き継いだか）を判断する土台として使う。
     断定できない項目は「未確認」のまま残してある。埋めるときは一次資料を添えること -->

## 比較のメタ情報

| 項目 | 値 |
|---|---|
| Boostnote-legacy の基準コミット | `789926bc760628ecedbd0051ca96d190a2be234f`（master HEAD、2021-09-02T01:17:08Z、「Update readme」マージ） |
| Boostnote-legacy の基準時点バージョン | package.json 上 0.16.1 |
| 最終リリースタグ（参考。基準には使わない） | 本リポジトリのタグ最大は `v0.16.0`（2020-07-20）。v0.9 以降のバイナリ配布は別リポジトリ `BoostIO/boost-releases` に移されており、そちらの最終は v0.16.1（2020-09-04） |
| haboku の HEAD | `05ce8db68d638e7c592355cac76004da7a604c6b`（2026-08-13T23:56:09+09:00、ブランチ `milestones-1-6`） |
| 調査日 | 2026-08-15 |
| haboku 側の未コミット変更 | 2 ファイル。`src/main.rs`（**タグの一覧・パレット表示の追加** + パレットのスクロール追従）と `docs/implementation-notes.md`（その記録）。タグ表示は今回の比較対象（タグ機能）に直接関わるため、**その旨を明記した上で実装状況に含めた**。それ以外の実装確認は HEAD 時点のコードに基づく |

補足: Boostnote-legacy 側の取得はすべて GitHub API / raw.githubusercontent.com 経由の読み取りで、
基準コミットのファイル実体（`readme.md`・`package.json`・`browser/` 配下のコード）を直接参照した。
haboku 側は `docs/spec.md`・`CLAUDE.md`・`docs/decisions/`・`src/` のコード読解による。
**`cargo test` は今回実行していない**（読み取り調査のみ。テストの存在はコード上で確認した）。

## 概要

Boostnote-legacy（旧名 Boostnote）は、BoostIO 社による**プログラマ向けのオープンソース・デスクトップメモアプリ**
（README 自己定義: "Note-taking app for programmers." "Apps available for Mac, Windows and Linux."）。
Electron + React + Redux 製で、Markdown ノートとコードスニペットノートの 2 種を、ローカルディレクトリ =
「storage」に **1 ノート 1 CSON ファイル**として保存する。複数 storage 管理・フォルダ・タグ・
Markdown プレビュー（数式・mermaid 等）・多形式エクスポートなど機能は広い。リポジトリは
**アーカイブ済み**で、README とリポジトリ説明文が後継の Boost Note（BoostNote-App）へ誘導している。

haboku と Boostnote-legacy の関係について、一次資料で裏付けられた事実は次のとおり。
**コードレベルの継承（フォーク）関係は無い。** haboku は Rust / iced 製で、`CLAUDE.md` に明記された
直接の前身は「gpui + gpui-component で書いた前身」（凍結済み、別リポジトリ）である。
一方、**設計上の参照点として Boostnote を意識していた証拠**はリポジトリ内に複数ある:

- `src/vault.rs:1-5` — 「`.md` が唯一の真実」方針の理由として「Boostnote が `.cson` を真実にしたせいで移行コストを払う羽目になった、その轍を踏まないための線引き」と明記（反面教師としての参照）
- `docs/decisions/0015-vault-location-setup.md` — 初回セットアップ画面を「Boostnote に倣い、既定候補を提示しつつ選び直せる形にする」と明記。同 ADR で「保存先の複数登録・切り替え（Boostnote の特徴）」を非目標として明示的に却下
- `docs/spec.md:21` — 非目標欄に「Boostnote は複数 storage を持てるが、初回に 1 つ決めれば今の要求は満たせる」
- `src/main.rs:2053, 4070`・`src/vault.rs:67` — セットアップ画面・既定フォルダ作成・一覧の既定順（最近いじった順）で Boostnote の挙動を参照するコメント

つまり両者の関係は「**UX と教訓のレベルで参考にした**」であり、コード・データ形式の互換性は無い
（CSON vs 素の `.md`）。これ以上の継承関係（例: gpui 前身が Boostnote をどう参照していたか）は
本リポジトリの一次資料からは**関係未確認**。

## 機能比較表

状態ラベル: 搭載 / 部分対応 / 外部サービス依存 / 計画のみ / 未確認。
Boostnote 列の根拠パスはすべて基準コミット `789926bc…` のもの。
haboku の「仕様（spec.md 上）」と「実装確認」は**別物として分けてある**（仕様に書かれていることは実装の証明ではない）。

| 機能 | Boostnote-legacy（`789926bc…`） | haboku の仕様（spec.md 上の扱い） | haboku の実装確認済み状態 | haboku で非目標にした理由（一次資料） |
|---|---|---|---|---|
| ノートの保存形式 | 搭載: 1 ノート 1 CSON ファイル（`notes/<key>.cson`、`@rokt33r/season` でパース。`createNote.js`, `resolveStorageNotes.js`） | 素の `.md` ファイル。「独自 DB もロックインも持たない」（spec 目的欄） | 実装済み: `src/vault.rs` が `.md` を直接読み書き。frontmatter は自前パース | 非目標ではなく方針転換。`src/vault.rs:1-5` が CSON の移行コストを明示的に反面教師にしている |
| Markdown ノート | 搭載（`MARKDOWN_NOTE` 型。`createNote.js`） | 搭載（アプリの目的そのもの） | 実装済み | — |
| スニペットノート（1 ノートに複数スニペット） | 搭載（`SNIPPET_NOTE` 型。`SnippetNoteDetail.js`） | 記載なし（非目標欄にも機能表にも登場しない） | 未実装 | 一次資料に却下理由の記述なし。**関係未確認**（単に対象外だった可能性が高いが、これは推測） |
| 複数 storage の登録・切り替え | 搭載（`addStorage.js` 等。メタデータは localStorage の `storages` キー） | **非目標**（spec 非目標欄） | 未実装: vault は 1 つ。`src/config.rs` が保存先 1 件を記憶 | ADR-0015・spec:21「初回に 1 つ決めれば今の要求は満たせる。選び直しは設定ファイルを消して起動すればできる」 |
| 保存先の初回セットアップ | 搭載に相当（storage 追加 UI。`StoragesTab.js`） | 搭載: `VAULT` → 記憶 → セットアップの 3 段（spec 機能表） | 実装済み: `Message::SetupUseSuggested` / `SetupBrowse` / `SetupPicked`（`src/main.rs`） | — 。むしろ **Boostnote に倣った**ことが ADR-0015 に明記されている（既定候補の提示 + 選び直し。既定パスは TCC の都合で `~/Documents/haboku`） |
| フォルダ | 搭載: storage 内の論理フォルダ（`boostnote.json` の `folders` 配列）。作成・改名・削除・並べ替え UI あり（`createFolder.js` 等） | 搭載: 「分類はフォルダが主」（spec 非目標欄）。3 ペインの 1 つ | **部分対応**: vault ルート直下の実ディレクトリを一覧・件数表示・絞り込み（`folder_pane`, `visible_indices`）。フォルダの作成・改名・削除 UI は無い | 一次資料に「フォルダ CRUD を作らない」という明示の記述は無し（**未確認**）。haboku のフォルダは実ディレクトリで、他ツール（Finder 等）で操作できる点が Boostnote と構造的に異なる |
| タグ | 搭載: タグ付け UI・色付き表示・`#タグ` 検索（`TagSelect.js`, ConfigManager の `coloredTags`, `search.js`） | 付け外し専用 UI は**非目標**。「frontmatter にあるタグで絞り込めれば足りる」（spec 非目標欄） | **部分対応**: frontmatter の `tags` パースは実装済み（`vault.rs`）。一覧・パレットへの**表示は未コミット変更で追加されたばかり**。**タグによる絞り込みは未実装**（`visible_indices` はフォルダのみ、検索はタイトルのみ）。パースには既知の取りこぼし 6 件あり（CRLF・単一スカラー等。未コミットの implementation-notes.md に記録、該当は `vault.rs:391,414-433`） | 付け外し UI の非目標理由は spec:20「frontmatter にあるタグで絞り込めれば足りる。分類はフォルダが主」 |
| 検索 | 搭載: スペース区切り AND の正規表現ベース・`#タグ`記法対応・全ノートを線形フィルタ（`browser/lib/search.js`）。ファジー検索・インデックスは無し | `Cmd+P` のファジー検索（スコア順上位 50 件、マッチ範囲はバイト範囲）。**全文検索は非目標** | 実装済み: `src/fuzzy.rs`（全角/半角・かなカナ正規化）+ `refilter()`。**対象はタイトルのみ**（README にも明記） | 全文検索の非目標理由は spec:18「約 1200 件を 30ms で読めているので、当面は素の走査で足りる」 |
| エディタ | 搭載: CodeMirror ^5.40.2。表エディタ（`@susisu/mte-kernel`）・スペルチェック（typo-js）・Prettier 整形 | iced `text_editor` + macOS Control 系編集操作（ADR-0016）+ Undo/Redo（ADR-0017） | 実装済み: `Message::Edit` / `CutToLineEnd` / `Undo` / `Redo`、テストあり | — |
| エディタ内シンタックスハイライト | 搭載に相当（CodeMirror + プレビュー側 highlight.js。詳細は未確認） | **spec.md の機能表に記載なし**（ADR-0009 に配色の経緯のみ） | 実装済み: `src/highlight.rs`（自前の Markdown 3 区分ハイライタ。syntect を配色都合で置き換え） | — （仕様と実装のずれの節を参照） |
| Markdown プレビュー / WYSIWYG | 搭載: markdown-it ^6.0.1 + katex・mermaid 8.5・flowchart・plantuml・chart.js 等のプラグイン群（package.json） | **非目標**（spec 非目標欄） | 未実装 | spec:23「書くのは Markdown のテキストそのもの」 |
| 自動保存 | **未確認**（今回の調査では保存タイミングのコードまで照合していない） | 搭載: デバウンス 1s / 上限 2.5s / 差分なしは書かない / fsync + アトミック rename / 失敗時バックオフ | 実装済み: `SaveGate`・`Message::Tick` / `Saved`、`vault.rs` の save（fsync → rename、権限引き継ぎ） | — |
| ごみ箱 | 搭載: `isTrashed` フラグ（ノートファイル内のフィールド。`createNote.js`） | `.trash/` へのファイル退避（確認なし、捨てる前に書き戻す） | 実装済み: `Message::DeleteNote`（`Cmd+⌫`） | — |
| スター（お気に入り） | 搭載: `isStarred` フラグ（`createNote.js`） | 記載なし | 未実装 | 一次資料に記述なし（**未確認**） |
| エクスポート（md/txt/html/pdf） | 搭載: ノート/フォルダ/タグ/storage 単位（`exportNoteAs.js`, `formatPDF.js` 等） | 記載なし | 未実装 | 一次資料に明示の却下理由なし（**未確認**）。ただし保存形式が素の `.md` であるため「md へのエクスポート」は概念として不要になっている（これは構造上の帰結であり、資料上の動機ではない） |
| URL からノート作成（Web クリップ） | 搭載: `createNoteFromUrl.js`（turndown で HTML→Markdown） | 記載なし | 未実装 | 記述なし（**未確認**） |
| 添付ファイル管理 | 搭載: `attachmentManagement.js` | 記載なし | 未実装 | 記述なし（**未確認**） |
| アプリ内蔵の同期・共有 | **実装確認できず**: 基準コミットのツリー全 588 パスに同期実装なし。「storage を Dropbox 等の同期フォルダに置く」運用は wiki 等の外部文書の話で、基準コミットの README/docs には記載なし | **非目標**（spec 非目標欄。「同期・共有・マルチデバイス」） | 未実装。外部変更との衝突検知も入れない方針 | spec:19「ローカル 1 台前提」。README「クラウドには一切つながない」。なお「無編集なら 1 バイトも書かない」設計により git/同期ツール配下の vault と共存できるとしている（README） |
| ブログ公開（WordPress） | 搭載（**外部サービス依存**。`PreferencesModal/Blog.js`） | 記載なし | 未実装 | 記述なし（**未確認**。「クラウドに一切つながない」方針からの帰結と読めるが、名指しの却下は無い） |
| 利用状況アナリティクス | 搭載（**外部サービス依存**、デフォルト有効。`AwsMobileAnalyticsConfig.js`、`amaEnabled: true`） | 該当機能なし | 実装なし（計測・通信コードは確認できず） | README「クラウドには一切つながない」 |
| WakaTime 連携 | 搭載（**外部サービス依存**。`wakatime-plugin.js`） | 記載なし | 未実装 | 記述なし（**未確認**） |
| ホットキーのカスタマイズ | 搭載: グローバルホットキー含む設定 UI（`ConfigManager.js`, `HotkeyTab.js`） | ショートカットは固定（`Cmd+P/N/R/⌫/Z` 等） | 実装済み（固定バインドのみ。カスタマイズ UI は無い） | 記述なし（**未確認**） |
| テーマ・カスタム CSS・多言語 UI | 搭載: テーマ切替・時間帯テーマ・カスタム CSS・`locales/` | 記載なし（配色は「幽玄」1 種。ADR-0009） | テーマ固定・日本語 UI のみ | ADR-0009 は配色の採用理由であり、テーマ切替を却下した記述ではない（**未確認**） |
| 自動アップデート | 搭載: `electron-gh-releases`、`autoUpdateEnabled: true` | 記載なし（配布自体が未着手: マイルストーン 8） | 未実装 | 記述なし（**未確認**） |

## 仕様と実装のずれ（haboku 側）

どちらが正かはここでは判断しない。両方を記す。

1. **タグによる絞り込み**: `docs/spec.md` 非目標欄は「frontmatter にあるタグで絞り込めれば足りる」と
   絞り込みを前提にした書き方だが、実装にはタグでの絞り込みが**存在しない**（絞り込みはフォルダのみ、
   検索対象はタイトルのみ）。タグは表示されるだけで、それも未コミット変更で入ったばかり。
2. **エディタのシンタックスハイライト**: `src/highlight.rs` として実装済み（frontmatter・見出し・コードの
   3 区分）だが、`docs/spec.md` の機能表に対応する行が**無い**。ADR-0009 は配色の判断のみ。
3. **frontmatter パースの既知の取りこぼし**: 未コミットの `docs/implementation-notes.md` に、`tags` パースの
   取りこぼし 6 件（CRLF ファイルで frontmatter 全無視、`tags: rust` 単一スカラー非対応等。
   該当 `src/vault.rs:391, 414-433`）が申し送りとして記録されている。spec の「frontmatter にあるタグ」
   仕様に対する実装の穴として把握しておく。
4. 上記以外の spec 機能表の各行（保存先の決定・vault 読み込み・一覧・ファジー検索・編集・自動保存・
   Undo/Redo・切替・リネーム・削除・終了）は、対応する `Message` バリアント・関数・テストの存在を
   コード上で確認した。ただし境界挙動の網羅検証（テスト実行）は今回行っていない。

## 差分から見える設計思想の違い

一次資料で裏付けられる範囲では、haboku が Boostnote-legacy との対比で明確に選び直したのは次の 3 点。

1. **データの真実をアプリの外に置く。** Boostnote は CSON という独自形式を「真実」にした。haboku は
   その移行コストを名指しで反面教師にし（`src/vault.rs:1-5`）、素の `.md` を唯一の真実とし、
   インデックス類は「再構築可能なキャッシュ」に限る線引きをした。エクスポート機能が要らないのは
   この選択の構造的な帰結でもある。
2. **storage の管理より、1 つの vault の即応性。** 複数 storage は「Boostnote の特徴」と認識した上で
   非目標にし（ADR-0015、spec:21）、代わりに 約 1200 件を 50ms 以内で開く・打鍵時 1ms 未満という
   数値目標（CLAUDE.md「Done の定義」）に投資している。一方、初回セットアップの UX
   （既定候補の提示 + 選び直し）は Boostnote から**引き継いだ**と ADR-0015 に明記されている。
3. **外部サービスへの接続をゼロにする。** Boostnote-legacy は基準コミット時点でアナリティクス
   （デフォルト有効）・WordPress 公開・WakaTime 連携を持つ。haboku は README で「クラウドには
   一切つながない」と宣言し、CLAUDE.md のセキュリティ最小則（Lethal Trifecta の回避）とも整合する。

以下は**推測**（一次資料に記述が無い）: プレビュー・スニペットノート・エクスポート等の広い機能群を
持たないのは、個別に却下した記録があるわけではなく、CLAUDE.md の「BSSN / YAGNI: 今の要求だけを解く」
という進め方の型により、そもそも要求に上がらなかった結果と考えるのが自然。また、Electron の性能や
Boostnote の動作速度への不満が動機だったという記述は一次資料に**存在しない**ため、性能目標
（50ms 起動等）を Boostnote への対抗と解釈することはできない。

## 参考文献

### Boostnote-legacy（すべて基準コミット `789926bc760628ecedbd0051ca96d190a2be234f`、取得日 2026-08-15）

- リポジトリメタデータ: https://api.github.com/repos/BoostIO/Boostnote-legacy （`archived: true`、full_name は `BoostIO/BoostNote-Legacy`、description が BoostNote-App へ誘導、最終 push 2023-04-19）
- master ブランチ: https://api.github.com/repos/BoostIO/Boostnote-legacy/branches/master
- タグ・リリース: https://api.github.com/repos/BoostIO/Boostnote-legacy/tags 、https://api.github.com/repos/BoostIO/boost-releases/releases/latest （v0.16.1、2020-09-04）
- ファイル実体（raw.githubusercontent.com/BoostIO/Boostnote-legacy/789926bc…/ 配下）: `readme.md`、`package.json`、`FAQ.md`、`browser/main/lib/ConfigManager.js`、`browser/lib/search.js`、`browser/main/lib/dataApi/{createNote,resolveStorageData,resolveStorageNotes,addStorage,exportNoteAs,createNoteFromUrl}.js`、`browser/main/modals/PreferencesModal/StoragesTab.js`
- 未確認のまま残した点: 正確なアーカイブ日時（API の `archived_at` が null）、Boostnote → Boostnote-legacy 改名の正確な時期、Dropbox 同期運用の公式言及（wiki 未照合）、Boostnote 側の自動保存挙動

### haboku（HEAD `05ce8db68d638e7c592355cac76004da7a604c6b` + 明記した未コミット変更、参照日 2026-08-15）

- `CLAUDE.md`（前身が gpui 版であることの明記、Done の定義、BSSN/YAGNI、セキュリティ最小則）
- `docs/spec.md`（目的・非目標・機能の関数化・マイルストーン）
- `docs/decisions/0002-drop-read-only-mode.md`、`0009-yugen-color-scheme.md`、`0015-vault-location-setup.md`、`0016-macos-control-key-bindings.md`、`0017-undo-redo.md`
- `README.md`、`src/vault.rs`、`src/fuzzy.rs`、`src/highlight.rs`、`src/config.rs`、`src/main.rs`
- 未コミット: `src/main.rs`（タグ表示 + パレットスクロール追従）、`docs/implementation-notes.md`（同件の記録と frontmatter パースの申し送り）
