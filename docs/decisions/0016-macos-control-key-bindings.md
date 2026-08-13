# ADR-0016: macOS の Control 系編集操作は、既定が取りこぼした分だけアプリで補う

- 日付: 2026-08-12
- ステータス: 採用
- 関連: ADR-0010（⌘ 混入。**同じ `editor_key_binding` に足す**）/ ADR-0004（subscription はイベントを消費できない）

## 文脈

<!-- どんな問題・制約の中で決めたか。評価軸の優先度を先に書く -->

本文エディタで **`Ctrl+A`（行頭）・`Ctrl+E`（行末）が効かない**。macOS では Cocoa の
テキスト部品がこの emacs 風キーバインドを標準で持っているので、「効かないほうが異常」に見える。

まず**どこが壊れているか**を先に確定させた（この題材の規律: 状態と表示のどちらが壊れているかを
数字で分ける）。結果、**壊れているのは haboku ではなく依存ライブラリ**だった。

`iced_widget-0.14.2/src/text_editor.rs` の `Binding::from_key_press`:

1. `convert_macos_shortcut`（同 1507 行）は **`Modifiers::CTRL` 完全一致のときだけ**
   `Ctrl+A → Named::Home`、`Ctrl+E → Named::End`、`B/F → Arrow`、`H → Backspace`、
   `D → Delete` へ変換し、`modified_key` に入れる（1205 行）
2. ところが変換後を見る `match` の分岐は `Enter` / `Backspace` / `Delete` / `Escape` の 4 つだけで、
   `Home` / `End` / `Arrow*` は `_` へ落ちる（1220 行）
3. `_` の中は**まず `text` を見る**。macOS の winit は `text_with_all_modifiers()` を返すので
   `Ctrl+A` の `text` は `Some("\u{1}")`。`text.chars().find(|c| !c.is_control())?` の **`?` が
   関数全体を `None` で終わらせる**（1222 行）
4. 仮にそこを抜けても、motion 判定が見るのは `modified_key` ではなく**変換前の `key`**
   （1225 行）。`Character("a")` は `Named` ではないので、やはり `None`

つまり**変換結果に到達する道が二重に塞がっている**。`Ctrl+H` だけは変換後が `Backspace` で
上の 4 分岐に当たるため、たまたま生きていた。

そして重要な帰結が 2 つある。

- **macOS 側の設定では直せない。** `text_editor` は Cocoa のネイティブ部品ではなく自前描画なので、
  システム設定も `~/Library/KeyBindings/DefaultKeyBinding.dict` も届かない。
  **アプリが自分で定義しなければならない項目**である
- **壊れているのは本文エディタだけ。** 同じ `convert_macos_shortcut` を使う `text_input.rs:1042` は
  制御文字で `return` せず素通しするので、パレット検索欄・リネーム欄の `Ctrl+A/E` は既に動く。
  直す範囲は `editor_key_binding` に閉じる

評価軸の優先度:

1. **見ていない本文を書き換えない**（ADR-0002 / ADR-0010 と同じ線）
2. macOS で当然効くべき編集操作が効く
3. 依存の既定に手を出す範囲を最小にする（iced が直したときに二重定義で衝突しない）

## 決定

`editor_key_binding` を「**既定が答えを出せなかったときだけ補う**」形にする。

```rust
match text_editor::Binding::from_key_press(kp) {
    Some(text_editor::Binding::Insert(_)) if command => None,   // ADR-0010
    Some(other) => Some(other),
    None if focused && control_only => macos_control_binding(&key),
    None => None,
}
```

補うのは `Ctrl` 単独 + `a`/`e`/`b`/`f`/`n`/`p`（カーソル移動）、`d`（後ろを 1 文字削除）、
`k`（行末まで切り取り）。

- **既定より後ろに置く。** 先に自前の表を引くと、既定が正しく処理している組み合わせを奪う
  （`Cmd+Ctrl+A` の `SelectAll`、`Ctrl+H` の `Backspace`、Linux の `Ctrl+A` = 全選択）。
  `#[cfg(target_os = "macos")]` で囲む代わりにこの順序で解いたので、**iced 側が将来直したら
  自前の分岐は自然に到達不能になり、挙動は変わらない**
- **`Modifiers::CTRL` 完全一致で見る。** `control()` だと `Ctrl+Shift+A` や `Cmd+Ctrl+A` まで拾う
- **フォーカスが無いときは補わない。** `key_binding` は**フォーカスの有無に関わらず呼ばれる**
  （`Status::Active` で来る）。既定は先頭でフォーカスを見て `None` を返すが、自前の分岐は
  自分で見ないと素通しになる。パレットを開いている間の `Ctrl+N`（一覧移動）で裏のカーソルが動き、
  `Ctrl+D` なら**見ていない本文から 1 文字消える**。これは ADR-0010 が塞いだ穴と同型
- **`Ctrl+K`（行末まで切り取り）だけは `Binding` を組み合わせず、`Binding::Custom` で
  `Message::CutToLineEnd` を投げて `update()` で処理する。** そこで
  「行末まで選択 → **選択が空でなければ**クリップボードへ書いて削除」を順に行う。
  行末・空行では何も起きない。行を繋げたいときは続けて `Ctrl+D` を押す（改行を 1 文字として消す）
- **`Ctrl+K` で消した分がクリップボードへ入るのは意図的。** kill ring（`Ctrl+Y` で貼り戻す）は
  作らない代わりに、`Cmd+V` を戻し道にする。**このアプリには Undo が無く**（iced 0.14 の
  `text_editor` 自体に無い）、自動保存は 1 秒で走るので、消しすぎを取り返す手段がこれしかない。
  代償はコピー済みの内容が上書きされること。取り消せない削除より、そちらを取った

## 却下した案

- **`Ctrl+K` を `Sequence[Select(End), Cut]` で組む。最初にこれを実装して、実機で落ちた。**
  **`Binding::Sequence` は状態を進めない。** `Select` は `shell.publish` でアクションを
  アプリへ送るだけで、`Content` はこの時点では変わらない（`text_editor.rs:855`）。一方 `Cut` は
  **その場で `content.selection()` を読む**（同 831 行）。同じ Sequence の中では
  Cut が見るのは選択前の状態＝常に空なので、**ガードに弾かれて何も消えず、選択だけが残る**。
  「`Cut` は空選択なら安全」という読みは正しかったが、**その安全装置が常時作動していた**。
  同じ理由で `Sequence[Select(End), Backspace]` は逆に危険側へ振れる（`Backspace` は
  ガードが無いので、選択が空のまま手前の 1 文字を消す）。**`Sequence` に組めるのは、
  直前の結果を読まない binding だけ**
- **`text_editor` を fork / iced を patch する。** 直すのは 1 行だが、依存を自前管理に
  引き上げるコストが釣り合わない。上流が直れば自前の分岐は死ぬだけで済む形にした
- **subscription（`event::listen_with`）で拾う。** subscription は**イベントを観測できても
  消費できない**（ADR-0004）。`Ctrl+D` を拾ってカーソルを動かしても、同じ打鍵が
  エディタにも届く。そもそも `key_binding` という正規の差し込み口がある
- **`from_key_press` を呼ばず全部を自前で定義する。** `Copy` / `Paste` / IME / 矢印まで
  抱えることになり、ADR-0010 が「一律に落とさない」と決めた理由をそのまま踏み直す
- **macOS のシステム設定で直してもらう。** 上記のとおり届かない。**利用者側の設定項目ではない**

## 帰結

- 本文で `Ctrl+A/E/B/F/N/P/D/K`（および既定由来の `Ctrl+H`）が効く。パレット検索欄・リネーム欄は
  iced の `text_input` 側で元から効いている
- **`Ctrl+K` は「binding の形」ではなく「本文がどう変わるか」でテストする。**
  最初の実装は形のテスト（`Sequence` の中身が `[Select(End), Cut]` であること）を通しておきながら、
  実機では選択されるだけで何も消えなかった。**形が正しくても効果が出ない**経路があるという証拠。
  今は `Message::CutToLineEnd` を流して `content.text()` を突き合わせている
- 行末の `Ctrl+K` は**本文を変えないだけでなく dirty も立てない**（立てると無編集で自動保存が走り、
  mtime が動く）。これもテストで固定した
- 回帰テストは **`text` が制御文字のとき（macOS 実機）と `None` のときの両方**で固定した。
  片方だけだと「テストは通るのに実機で効かない」実装が通る
- **フォーカスの無いエディタが Control 系に反応しないこと**もテストで固定した
- 修正を外すとテストが落ちることを確認済み（負のコントロール）。GUI の修正は目視だけでは
  「たまたま動いた」を区別できない
