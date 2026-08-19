# ADR-0021: 終了操作を保存ガードに乗せる — Quit は `performClose:` へ付け替え、Cmd+W は Rust で受ける

- 日付: 2026-08-18
- ステータス: 採用・**実装済み**（2026-08-19。受け入れ条件はすべて満たした — 下の「実装の結果」）
- 関連: ADR-0007（終了時の保存）、ADR-0012（2 段階クローズと退避）、ADR-0014（`PendingAction::Close`）、
  `docs/review-2026-08-17-public-release.md` のブロッカー B-1（この ADR はその解消方針）

## 文脈

<!-- どんな問題・制約の中で決めたか。評価軸の優先度を先に書く（総合点方式の罠を避ける） -->

公開可否レビュー（2026-08-17）の唯一のブロッカー。**実機で再現済み**
（使い捨て vault + release ビルドに PID 直指定の CGEvent。編集は Cmd+N → Return×2、
最後の編集から 0.2 秒後 = デバウンス 1 秒の内側で終了操作）:

| 終了操作 | プロセス | 打った編集 |
|---|---|---|
| ✕ ボタン | 0.4 秒で終了 | 保存された（ノート 2 bytes） |
| Cmd+W | **窓ごと生存** | （不発。2 bytes は 1 秒後の自動保存によるもの） |
| Cmd+Q | **0.2 秒で即死** | **消えた（ノート 0 bytes）** |

原因は 3 つとも winit 0.30.13 のソースで確認した:

- `WindowEvent::CloseRequested` の発生源は `windowShouldClose:` **ただ 1 箇所**
  （`platform_impl/macos/window_delegate.rs:143-148`）。ここを通るのは ✕ = `performClose:` だけ
- 既定メニューの Quit 項目は **`terminate:` セレクタ + キーエコー "q"**（同 `menu.rs:66-73`）で、
  NSApp を直接終了させる。winit は `applicationShouldTerminate:` を実装していないので差し止められない
  （あるのは取り消し不能な `applicationWillTerminate:` のみ。同 `app_state.rs:69`）
- 既定メニューに **Close 項目が存在しない**ため、Cmd+W はキーエコー先を持たず不発。
  iced_winit 0.14.0 は winit の `default_menu` に触れる口を公開していない（src 全域 grep でゼロ件）

つまり ADR-0007 が「最後のデータ喪失の穴」として塞いだ終了経路のうち、
**テストで固定できるのは `CloseRequested` から先だけ**で、OS がそれを届けるかどうかは
テストの外にあった。`src/main.rs:450-455` の「Cmd+W・✕・Cmd+Q が届く」というコメントは
3 つのうち 2 つが実態と食い違っている。

評価軸の優先度（ADR-0007 / 0012 / 0014 から変えない）:

1. **ノートを失わないこと**
2. 操作の結果が予測できること
3. 既定メニューの機能（About / Services / Hide）と既存テストを壊さないこと

## 決定

<!-- 何を選んだか1〜3行 -->

**2 本柱。メニューは作り直さず、Quit 項目の飛び先だけを付け替える。**

### 柱 1: Quit 項目の action を `terminate:` → `performClose:` に付け替える（objc2、約 20 行）

起動直後に一度だけ app メニューを走査し、`action == terminate:` の項目を
`setAction(performClose:)` + `setTarget(nil)` に書き換える。nil ターゲットの action は
レスポンダチェーン（= キーウィンドウ）に届くので、**Cmd+Q もメニューの「Quit haboku」クリックも、
✕ ボタンと同一の経路**（`windowShouldClose:` → `CloseRequested` → `begin_save` →
保存 / 拒否 / `.rescue`）になる。保存が済めば窓が閉じ、iced は最後の窓が閉じると終了する
（✕ の実機実験で確認済み）ので「Quit = 保存してから終了」の意味は保たれる。

- **実行タイミング**: winit はメニューを最初のイベント配送前（`applicationDidFinishLaunching`）に
  取り付けるので、起動後ならいつでも書き換えられる。iced 0.14 の boot は `(App, Task<Message>)` を
  返せる（`iced-0.14.0/src/application.rs:562` の `IntoBoot` 実装で確認）ので、boot から
  `Task::done(Message::RetargetQuitMenu)` を流し、**update() の中（= メインスレッド保証）で
  1 回だけ**パッチする。`MainThreadMarker::new()` でガードする
- **失敗したら黙らない**: 付け替えに失敗したら常駐エラー行に出す。その場合の挙動は
  「今日と同じ」に劣化するだけで、悪化はしない
- **依存**: `objc2` 0.6.4 / `objc2-app-kit` 0.3.2 を直接依存に昇格する。**新しい依存ではない**
  （winit 経由で既にツリーにいる。Cargo.lock で確認）。objc2-app-kit は default-features を切り、
  `NSApplication` / `NSMenu` / `NSMenuItem` の granular feature だけ有効にする

### 柱 2: Cmd+W は純 Rust で `CloseRequested` に写像する（AppKit 不要・テスト可能）

Cmd+W はメニューに食われず**普通のキーイベントとしてアプリまで届いている**（不発の原因は
届け先が無いことだった）。`subscription` の `listen_with` クロージャ（`src/main.rs:1837`。
第 3 引数で window Id が取れる）を名前付き関数に切り出し、
**Cmd+W（修飾は `COMMAND` 完全一致・`repeat: false` のみ）→ `Message::CloseRequested(window)`**
の写像を足す。メニュー項目は増やさない。

- **修飾は完全一致**: `Cmd+Shift+W` 等を拾わない（ADR-0016 の `Ctrl` 完全一致と同じ流儀）
- **repeat を無視する理由**: 保存失敗中の 2 段階クローズ（1 回目拒否 → 2 回目退避）が、
  キー押しっぱなしのオートリピートで警告を素通りしないようにするため
- 素の "w" は今までどおり `Message::Key` として流れ、本文に入る

### 受け入れ条件（これを満たすまでレビュー B-1 は閉じない）

- `cargo test` / `cargo clippy --all-targets` 通過（写像関数のユニットテストを含む:
  Cmd+W → CloseRequested / repeat 無視 / 素の w は Key / Cmd+Shift+W は不発）
- **実機ドライバ 3 経路（Cmd+Q / Cmd+W / ✕）すべてで「プロセス終了 + ノートが 2 bytes」**
- docs 更新: `src/main.rs:450-455` のコメント / `docs/spec.md:53` の終了行 /
  README のキー表（`Cmd+W` 追記）/ レビュー B-1 に解消の追記

### 実装の結果（2026-08-19）

満たした:

- `cargo test` 112 passed / 0 failed、`cargo clippy --all-targets` 警告なし。
  写像のユニットテストは 4 本（`cmd_w_asks_to_close_the_window` /
  `a_held_down_cmd_w_does_not_close_twice` / `a_bare_w_is_still_typed` /
  `cmd_shift_w_does_not_close_the_window`）+ ADR-0010 の回帰 1 本
- 実機ドライバ 3 経路。**設計時に「終了 + 2 bytes」だけを条件にしていたのは不十分だった**ので、
  終了までの経過時間を足した（下記）
- docs: `src/main.rs` の `CloseRequested` の doc / `docs/spec.md` の終了行 /
  README のキー表と「前提と割り切り」/ レビュー B-1 に追記

| 経路 | プロセス | 終了まで | ノート |
|---|---|---|---|
| 放置（煙試験） | 生存 | — | 2 bytes（自動保存。計器の正当性確認） |
| Cmd+Q | **終了** | 0.12s | **2 bytes**（`0a0a`） |
| Cmd+W | **終了** | 0.12s | **2 bytes** |
| ✕ ボタン | **終了** | 0.14s | **2 bytes** |

**時間を測ったのは、「2 bytes」だけでは書いた主体を特定できないから。** 終了が遅ければ
デバウンス（1 秒）が先に満期を迎え、保存ガードが素通りでも自動保存が同じ 2 bytes を残す。
実測 0.12〜0.14 秒（最後の編集からでも約 0.45 秒）はどちらの満期よりも手前なので、
書いたのは閉じる要求の保存ガードだと分離できる。**受け入れ条件を「終了 + 2 bytes」と
書いた時点では、この偽陽性に気づいていなかった。**

`objc2` / `objc2-app-kit` の追加で **Cargo.lock に増えた `[[package]]` はゼロ**。動いたのは
haboku 自身の依存欄の 2 行だけで、crate はどちらも winit 経由で既にツリーにいたものを
名指ししただけ。「新しい依存ではない」の裏取り。

実機ドライバの手順（再構築できる粒度で）: 使い捨て vault + release ビルドを起動し、
Swift の `CGEvent.postToPid`（AX trusted 必須。System Events は同名プロセス誤解決の罠が
あるので使わない）で Cmd+N → Return×2（IME の影響を受けずに dirty を作れる打鍵）→
0.2 秒後に終了操作。判定は作られた `.md` のバイト数（2 = 保存された / 0 = 消えた）。
✕ は AX API の `kAXCloseButtonAttribute` を `AXPress` する。

## 却下した代替案

<!-- 何を・なぜ見送ったか。「なぜこれでないのか」が将来いちばん効く記録 -->

- **winit の `with_default_menu(false)` で既定メニューを消し、Cmd+Q も純 Rust で受ける。**
  メニューが無ければ Cmd+Q はキーイベントとして届くので objc2 が要らなくなる。却下したのは
  **iced 0.14 がこの口を公開していない**ため（iced_winit 0.14.0 の src 全域に該当トークンなし）。
  iced を迂回して event loop を自前構築するのはこの問題に対して過大。About / Hide も失う
- **`applicationShouldTerminate:` を winit のデリゲートクラスへ `class_addMethod` で注入する。**
  Dock 右クリックの Quit・ログアウト・AppleScript quit（すべて生の `terminate:`）まで塞げる
  正攻法だが、**winit 内部クラスへの実行時パッチは壊れやすい**。防げる追加損失は、通常運転なら
  自動保存上限の 2.5 秒以下に有界。今回は見送り、**再導入条件**を残す:
  (1) Dock Quit / ログアウト起因の消失報告が実際に来た
  (2) winit が `applicationShouldTerminate` を公式に扱うようになった
- **NSMenu でメニューを全置換する。** About / Services / Hide を作り直す手間だけ増え、
  得るものが無い。付け替え 1 箇所で足りる
- **Cmd+W を Close メニュー項目として足す。** macOS 標準の形だが、このアプリの
  ショートカットは全部 Rust 側の表で管理している（`Message::Key` の ⌘ 表）。メニュー項目は
  第二の dispatch 経路を作り、しかもユニットテストから届かない。Rust 側なら既存のテスト基盤
  （`send` / `flush_saves`）でそのまま固定できる

## 帰結

<!-- この決定で背負うトレードオフ -->

- **最小化中は Quit 項目が灰色になる**（nil ターゲットの action は、レスポンダチェーンに
  実装者がいないとメニュー検証が項目を無効化する）。最小化中は編集もできない = 新しい dirty が
  生まれないので、データ喪失リスクは増えない。Dock から戻して閉じれば済む
- **Dock 右クリックの Quit・ログアウト・AppleScript quit は引き続き素通り。** 損失は自動保存
  上限の 2.5 秒以下に有界。無界になるのは保存が失敗し続けている間だけで、そのときは
  常駐エラーと未保存マーカーが画面に出ている（再導入条件は却下欄）
- セットアップのフォルダ選択パネルを開いたまま Cmd+Q すると、キーウィンドウがパネルなので
  パネルが閉じる（もう一度 Cmd+Q で本体が閉じる）。風変わりだが安全側
- メニュークリックの「Quit haboku」も保存を待つようになる。保存が失敗していれば
  1 回目は断られる — **Quit が一発で効かないことがある**のは、2 段階クローズ（ADR-0012）の
  仕様どおりの姿
- `Message` が 1 つ増える（`RetargetQuitMenu`）。テストがこのメッセージを送らない限り
  AppKit には触れないので、既存の `boot()` / `update()` 直呼びテストは無傷

## 学び

**テストが固定できるのは、イベントが自分のコードに入ってから先だけ。** 終了ガードは
テストで「公開水準を超える」と判定していたが、OS がそのイベントを届けるかどうかは
実機でしか確かめられなかった。安全装置の検証は「装置が動くか」と「装置まで信号が来るか」の
2 段で考える。

**macOS のショートカットは「キーイベント」ではなく「メニュー項目のキーエコー」として
動くことがある。** Cmd+W が不発（項目が無い）で Cmd+Q が即死（項目が `terminate:` を送る）という
非対称は、どちらも**メニューに項目があるか・その action が何か**だけで決まっていた。
クロスプラットフォームの GUI ライブラリでは、この macOS の作法が既定メニューの中身に隠れる。
