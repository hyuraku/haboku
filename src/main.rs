//! haboku 本体。3 ペイン（フォルダ / 一覧 / エディタ）+ デバウンス自動保存。
//!
//! 実行: `VAULT="$HOME/Documents/haboku" cargo run --release`
//!
//! **必ず release で動かすこと。** debug だと vault 読み込みの数字が一桁変わる
//! （実測: cold 185.1ms / warm 32.8ms @ release・約 1200 件）。

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iced::keyboard;
use iced::widget::{
    button, column, container, markdown, mouse_area, rich_text, row, scrollable, span, stack,
    text, text_editor, text_input,
};
use iced::{Color, Element, Fill, Font, Length, Subscription, Task};

use haboku::{config, fuzzy, vault};

mod appkit;
mod highlight;

// ── 幽玄パレット。6 トークン + 派生値（ADR-0009、値の正は意匠 Artifact）──
//
// 琥珀だけが「今どこにいるか」の合図で、枯茶は警告専用。赤・青は使わない。
// 派生値（soft / wash / hairline）は 6 トークンからアルファと明度だけで作る。

/// 濃墨。地の色（ウィンドウ・サイドバーの底）。
const KOBOKU: Color = Color::from_rgb8(0x18, 0x16, 0x1A);
/// 淡墨。一段浮いた面（エディタ・オーバーレイのパネル）。
const TANBOKU: Color = Color::from_rgb8(0x2A, 0x26, 0x2C);
/// 生成り。主文字。
const KINARI: Color = Color::from_rgb8(0xE9, 0xE3, 0xD5);
/// 霞。副文字（件数・プレビュー・注釈・ステータス行）。地・淡墨の両方に 4.5:1 以上。
const KASUMI: Color = Color::from_rgb8(0x9E, 0x96, 0x89);
/// 琥珀。唯一のアクセント（選択・保存済・検索の一致文字）。
const KOHAKU: Color = Color::from_rgb8(0xC8, 0x9C, 0x62);
/// 琥珀の明るい側。一致文字・選択中タイトル・「保存しました」。
const KOHAKU_SOFT: Color = Color::from_rgb8(0xD8, 0xB9, 0x88);
/// 枯茶。警告・未保存・エラー。**赤は使わない。** 地に対し 4.5:1 を確保した値。
const KARACHA: Color = Color::from_rgb8(0xC1, 0x7F, 0x4E);

/// 琥珀の薄い wash。選択中の行の背景。
const KOHAKU_WASH: Color = Color { a: 0.14, ..KOHAKU };
/// 琥珀の濃い wash。エディタの選択範囲。
const KOHAKU_WASH_STRONG: Color = Color { a: 0.26, ..KOHAKU };
/// 生成りの 18% ヘアライン。パネルの枠線。
const HAIRLINE_STRONG: Color = Color { a: 0.18, ..KINARI };

/// 幽玄テーマを組む。iced の標準ウィジェット（text_input 等）は
/// この Palette から導出された extended palette で自動的に配色される。
fn yugen_theme() -> iced::Theme {
    iced::Theme::custom(
        "幽玄",
        iced::theme::Palette {
            background: KOBOKU,
            text: KINARI,
            primary: KOHAKU,
            success: KOHAKU_SOFT,
            warning: KARACHA,
            danger: KARACHA,
        },
    )
}

/// 選択中の行（フォルダ・ノート一覧）。`button::primary` の置き換え（ADR-0009）。
///
/// primary のベタ塗りは彩度で目を引きすぎる。琥珀の薄い wash を敷くだけにして、
/// 文字は生成りのまま「選ばれている」ことだけを語らせる。ホバーでも変えない
/// （選択済みの行にホバーの反応は要らない）。
fn selected_row(_theme: &iced::Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(KOHAKU_WASH)),
        text_color: KINARI,
        border: iced::border::rounded(3),
        ..button::Style::default()
    }
}

/// エディタ面。淡墨（一段浮いた面）に生成りの文字。枠は描かない
/// （ペインの区切りは濃墨との明度差だけで見せる）。
fn editor_style(_theme: &iced::Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Background::Color(TANBOKU),
        border: iced::Border::default(),
        placeholder: KASUMI,
        value: KINARI,
        selection: KOHAKU_WASH_STRONG,
    }
}

/// オーバーレイ（パレット・リネーム）のパネル。淡墨のカード面 + ヘアラインの枠。
fn panel_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(TANBOKU.into()),
        border: iced::Border {
            color: HAIRLINE_STRONG,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// [`highlight::Kind`] を色と書体に変換する。区分と意匠の対応はここに集約する。
fn markdown_format(
    kind: &highlight::Kind,
    _theme: &iced::Theme,
) -> iced::advanced::text::highlighter::Format<Font> {
    let color = |c| iced::advanced::text::highlighter::Format {
        color: Some(c),
        font: None,
    };
    match kind {
        // frontmatter はメタ情報。本文より一段引かせる。
        highlight::Kind::Frontmatter => color(KASUMI),
        // 見出しは明朝（名指し。ADR-0009 / ADR-0003 と同じ判断）。色は本文と同じ生成り。
        highlight::Kind::Heading => iced::advanced::text::highlighter::Format {
            color: None,
            font: Some(HEADING_FONT),
        },
        // コードは琥珀の明るい側。地の色は変えられない（Format は色と書体だけ）。
        highlight::Kind::Code => color(KOHAKU_SOFT),
    }
}

// ── 自動保存の閾値。CLAUDE.md の Done の定義がそのまま数値になっている ──

/// 打鍵が止まってから保存するまでの待ち時間。
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(1000);

/// 打ち続けている間の保存間隔の上限。
///
/// デバウンスだけだと**打鍵が止まらない限り永久に保存されない**。
/// dirty がこの時間に達したら打鍵の途中でも書く。
const AUTOSAVE_MAX_WAIT: Duration = Duration::from_millis(2500);

/// 「未保存」表示を出すまでに dirty が続く時間。**必ず `AUTOSAVE_MAX_WAIT` より長くする。**
/// 正常時は必ず上限以内に dirty が解けるので、ここへ到達すること自体が
/// 「自動保存が動いていない」という異常のシグナルになる。
const DIRTY_MARKER_DELAY: Duration = Duration::from_millis(3000);

/// 「保存しました」を消すまでの時間。
const SAVED_FLASH: Duration = Duration::from_millis(1500);

/// dirty を監視する間隔。dirty でない間は subscription ごと止まる。
const TICK: Duration = Duration::from_millis(100);

/// 保存が失敗したあと、自動保存が次に試すまでの最短の待ち時間。
const SAVE_RETRY_MIN: Duration = Duration::from_millis(500);

/// 同じく最長。失敗が続いても、これ以上は間隔を空けない
/// （直ったことに気づくまでが長すぎると「保存されないアプリ」になる）。
const SAVE_RETRY_MAX: Duration = Duration::from_secs(30);

/// パレットの入力欄を名指しする ID。開いた瞬間にフォーカスを飛ばすのに要る。
const PALETTE_INPUT_ID: &str = "palette-input";

/// パレットの候補リストを名指しする ID。選択に合わせてスクロールさせるのに要る。
const PALETTE_LIST_ID: &str = "palette-list";

/// エディタを名指しする ID。`Cmd+N` の直後にフォーカスを飛ばすのに要る。
const EDITOR_ID: &str = "editor";

/// リネームの入力欄を名指しする ID。
const RENAME_INPUT_ID: &str = "rename-input";

/// 新規ノートのファイル名。秒精度なので同一秒の連打は衝突する
/// （`vault::reserve_unique` が枝番を付けて防ぐ）。
const NEW_NOTE_NAME_FORMAT: &str = "%Y-%m-%d-%H%M%S";

/// パレットに一度に描く最大件数。約 1200 件を全部並べても人は読まない。
/// 絞り込むための道具なので上位だけ出せば足りる。
const PALETTE_MAX_RESULTS: usize = 50;

/// 本文を探し始めるクエリの長さ。**1 文字では探さない。**
/// 1 文字の連続一致はほぼ全ノートに当たって情報量がゼロで、しかも検索は必ず
/// 1 文字目を通る。ここが打鍵あたりで最も頻度の高い無駄になる（ADR-0018）。
const BODY_MIN_QUERY_CHARS: usize = 2;

/// サイドバー（フォルダ）の幅。
const SIDEBAR_WIDTH: f32 = 180.0;
/// ノート一覧の幅。
const LIST_WIDTH: f32 = 320.0;

/// 開いているパレットの状態。閉じているときは `None`。
struct Palette {
    query: String,
    /// 絞り込み結果。
    ///
    /// **`view()` では絞り込みを一切やらない。** クエリが変わった時だけ計算してここに置く。
    /// 打鍵のたびに全ノートを走査する穴（前身の `render()` で開けた穴）を塞ぐため。
    matches: Vec<Hit>,
    /// いま選んでいる `matches` の位置。
    selected: usize,
}

/// パレットの 1 行。**タイトルに当たったのか本文に当たったのかで見せ方が変わる。**
struct Hit {
    /// `App.notes` のインデックス。
    note: usize,
    kind: HitKind,
}

enum HitKind {
    /// タイトルに当たった。範囲は**タイトル基準**のバイト範囲。
    Title(fuzzy::Match),
    /// タイトルには当たらず、本文に当たった。
    ///
    /// `hit` は `snippet` 基準のバイト範囲（`raw` 基準ではない）。本文の一致は
    /// 連続一致なので範囲は必ず 1 本で、`Vec` は要らない。
    Body {
        snippet: String,
        hit: std::ops::Range<usize>,
    },
}

struct App {
    /// vault のルート。保存後に `parse_note` へ渡すのに要る。
    root: PathBuf,
    /// 保存先が決まっていない間の初回セットアップ。決まっていれば `None`（ADR-0015）。
    ///
    /// **`Some` の間はディスクに触らない。** `update()` の入口で他のメッセージを全部落として
    /// いるのは、まだ `root` が「候補」でしかないため（そこへ書くと意図しない場所に書く）。
    setup: Option<Setup>,
    /// 選んだ保存先を覚えておくファイル。**フィールドに持つのはテストのため**
    /// （`$HOME` を書き換えずに使い捨てのパスへ差し替えられる）。
    config_file: PathBuf,
    /// 全ノート。**読み込み時の順序（更新日時の新しい順）から動かさない。**
    ///
    /// 保存のたびに並べ替えると、編集中のノートが毎回先頭へ飛んで `selected` の指す先がズレる。
    /// 一覧が打鍵のたびに踊るのも実用上つらい。順序が更新されるのは次回起動時でよい。
    notes: Vec<vault::Note>,
    /// フォルダごとの件数。**起動時に1回だけ**数える。
    ///
    /// 前身ではこれを `render()` の中でやっていて、1 文字打つたびに全ノートを走査していた。
    /// iced でも `view()` に置けば同じ穴が開く。「毎フレーム走る場所に集計を置かない」は
    /// フレームワークに依らない教訓。
    folders: Vec<(String, usize)>,
    /// 絞り込み中のフォルダ。`None` は全件。
    selected_folder: Option<String>,
    /// いま一覧に出す `notes` のインデックス。**絞り込みは `view()` でやらない。**
    /// フォルダ選択が変わった時だけ作り直す。
    visible: Vec<usize>,
    /// 開いているノート（`notes` のインデックス）。
    selected: Option<usize>,
    content: text_editor::Content,
    /// 浮きパレット（`Cmd+P` のノート検索）。開いていないときは `None`。
    palette: Option<Palette>,
    /// リネーム入力中の名前（`Cmd+R`）。開いていないときは `None`。
    /// パレットとは同時に開かない（開くときに互いを畳む）。
    rename: Option<String>,
    /// 読み取り専用プレビュー（`Cmd+Shift+V`。ADR-0019）。`None` は編集モード。
    ///
    /// **パース済みの結果を持つ。** `view()` でパースすると打鍵・フレームのたびに全文パースが
    /// 走る（Done の定義: `view()` 構築 1ms 未満）。作り直すのはトグルで開いた瞬間と、
    /// 本文が差し替わる瞬間（`replace_content`）だけ。プレビュー中はエディタが view に
    /// 無いので編集は起こらず、この 2 点だけで表示と本文はズレない。
    preview: Option<markdown::Content>,
    /// いま押されている修飾キー。**`text_input` への混入を弾くためだけに持つ**（ADR-0010）。
    ///
    /// `text_editor` は `key_binding` で塞げるが、`text_input` に同じ差し込み口は無い。
    /// そこで「⌘ を押している間に来た入力は受け取らない」で塞ぐ。⌘ の keydown は
    /// 文字キーより必ず先に届くので、**同一イベント内の処理順に依存しない**。
    modifiers: keyboard::Modifiers,

    // ── 自動保存 ────────────────────────────────────────────
    /// dirty になった時刻。**「未保存の編集があるか」はこれ 1 つで表す**
    /// （別に `dirty: bool` を持つと、片方だけ更新するコードが書ける）。
    /// 上限判定と「未保存」表示にも使う。
    ///
    /// **打鍵のたびに更新してはいけない。** false → true の遷移でだけ記録する。
    /// 毎回更新すると「打ち続けている間の経過時間」が測れず、上限に永遠に届かない。
    dirty_since: Option<Instant>,
    /// 最後に編集された時刻。デバウンスの判定に使う（こちらは毎回更新する）。
    last_edit: Instant,
    /// 「未保存」を実際に描くか。dirty とは別物で、**異常が疑われるときだけ true**。
    show_marker: bool,
    /// 保存に失敗した等の常駐エラー。消えないこと自体がシグナル。
    error: Option<String>,

    // ── undo / redo（ADR-0017）──────────────────────────────
    //
    // iced 0.14 の `text_editor` は undo を持たない（下層の cosmic-text は `Change` を
    // 持つが、iced が入口を塞いでいる）。だから本文とカーソルの**全文スナップショット**を
    // 自前で積む。1 ノートは数 KB で、vault 全体を常時メモリに載せているこのアプリなら誤差。
    /// 元に戻せる状態。**ノート単位**で、本文を差し替えるときに畳む（`replace_content`）。
    undo: Vec<Snapshot>,
    /// やり直せる状態。**新しい編集で捨てる**（分岐した歴史は持たない）。
    redo: Vec<Snapshot>,
    /// いま continuing 中の編集の種類。**これが変わった瞬間が undo の区切り**。
    ///
    /// `None` は「次の編集から新しいまとまりを始める」印。カーソルが動いたときと
    /// undo / redo の直後に倒す（離れた場所の編集が 1 ステップに混ざらないように）。
    edit_group: Option<EditKind>,
    /// 保存できずに閉じるのを一度断ったか。**2 回目の要求で退避して閉じる**ための記憶。
    ///
    /// 保存が通れば `clear_dirty` で畳む。問題が直ったあとの初回はまた警告から始めたい
    /// （立てっぱなしだと、次に別の失敗を踏んだとき警告なしで終了してしまう）。
    close_refused: bool,
    /// 自動保存が続けて失敗している回数。バックオフの段数（`save_retry_delay`）。
    /// **数えるのは自動保存だけ。** 切替・終了などユーザーの操作は明示的な再試行なので待たせない。
    save_failures: u32,
    /// 自動保存を次に試してよい時刻。失敗のたびに後ろへ倒す。
    ///
    /// これが無いと、保存が失敗し続ける間 `should_save` が永久に真になり、
    /// 100ms ごとに同期 I/O を UI スレッドで回して画面が固まる。
    retry_after: Option<Instant>,
    /// 「保存しました」を消す時刻。
    saved_flash_until: Option<Instant>,
    /// 実際にディスクへ書いた回数。**検証用の計器。**
    ///
    /// 何も編集していないのにこれが増えるなら、`Content::text()` が元ファイルと違う
    /// 文字列を返している。mtime が動き続けて外部ツールから更新と誤認されるので、
    /// 画面に出して即座に気づけるようにする。
    saves: usize,
    /// 起動時の vault 読み込み時間。Done の定義（50ms）を常に目視できるようにする。
    load_ms: f64,
    /// 直前の `view()` 構築にかかった時間（マイクロ秒）。**恒久の計器。**
    ///
    /// 前身では `render()` の中でタグ集計をしていて、1 文字打つたびに全ノートを走査していた。
    /// iced でも `view()` に重い処理を置けば同じ穴が開く。Done の定義は「打鍵時の `view()`
    /// 構築が 1ms 未満」なので、常に画面に出して**書いた瞬間に気づける**ようにしておく。
    ///
    /// `view()` は `&App` しか取れないので `Cell` で内部可変にする。表示されるのは
    /// 1 フレーム前の値（構築中の時間は構築が終わるまで確定しない）。
    last_view_us: Cell<u128>,

    // ── 非同期保存 ──────────────────────────────────────────
    /// 進行中の保存。**同時に 1 本しか走らせない**（ADR-0014）。
    ///
    /// 直列にしているおかげで「古い保存の完了が新しい保存を上書きする」順序事故が
    /// **起こり得ない形**になっている。世代番号を持たないのはそのため。
    /// `notes` を触る操作（作成・削除・リネーム）は全部この後ろに並ぶので、
    /// ここに控えた `index` が完了時にズレることもない。
    saving: Option<InFlight>,
    /// 保存が通ったらやること。**保存に失敗したら捨てる**（それが従来のガードの中身）。
    ///
    /// 後から来た操作で上書きする。ノート A を選んですぐ B を選んだら、行き先は B でよい。
    pending: Option<PendingAction>,
}

/// 初回セットアップの状態（ADR-0015）。**保存先が決まるまでの画面**。
struct Setup {
    /// 提示する候補（`~/Documents/haboku`）。「別の場所を選ぶ…」の初期位置にも使う。
    suggested: PathBuf,
    /// 採用に失敗した理由。**ここに出す**（`eprintln!` は `.app` では誰にも見えない）。
    ///
    /// 保存先を作れない・読めないのは大抵 macOS の許可を拒否したときで、
    /// 黙って空の vault を開くとノートが消えたようにしか見えない（ADR-0002 と同じ穴）。
    error: Option<String>,
}

/// 起動時に保存先をどう決めたか（ADR-0015）。
#[derive(Debug, PartialEq)]
enum VaultChoice {
    /// そのまま開ける。
    Ready(PathBuf),
    /// 初回セットアップ画面を出す。
    NeedsSetup { suggested: PathBuf },
    /// 起動せず終わる。**明示的に指定されたものが外れているとき**だけここへ来る。
    Refuse(String),
}

/// 進行中の保存が書いている中身。完了メッセージには結果しか載せず、突き合わせはここでやる。
struct InFlight {
    /// 書き戻す先の `notes` インデックス。
    index: usize,
    path: PathBuf,
    /// **書き出した瞬間のスナップショット。** 完了時にエディタの現在値と比べて、
    /// 保存中に打たれた分があるかを判定する（あれば dirty を畳まない）。
    contents: String,
    /// 自動保存として始まったなら、その tick の時刻。ユーザー操作なら `None`。
    ///
    /// バックオフを数えるのは自動保存だけ（ADR-0012）。完了時に `Instant::now()` を
    /// 読まずに済むよう、**判定の基準時刻を持ち回す**（境界をテストから組み立てるため）。
    autosave_at: Option<Instant>,
}

/// 保存の完了を待ってから実行する操作。**どれもエディタの内容を捨てる操作**なので、
/// 保存が通るまで走らせてはいけない（従来 `if !save_now()` で塞いでいたもの）。
#[derive(Debug, Clone)]
enum PendingAction {
    /// ノートを開く。`from_palette` は `Cmd+P` 経由かどうか（パレットを畳み、
    /// 開く先が絞り込みで隠れるなら絞り込みも解く）。
    OpenNote { index: usize, from_palette: bool },
    NewNote,
    Rename(String),
    Delete,
    Close(iced::window::Id),
}

#[derive(Debug, Clone)]
enum Message {
    /// フォルダを選ぶ。`None` は全件表示に戻す。
    FolderSelected(Option<String>),
    /// ノートを開く（`notes` のインデックス）。
    NoteSelected(usize),
    Edit(text_editor::Action),
    /// `Ctrl+K`。カーソルから行末までを切り取る（ADR-0016）。
    ///
    /// **`Binding::Sequence` では書けないのでメッセージにしてある。** 理由は `update()` 側の doc。
    CutToLineEnd,
    /// `Cmd+Z` / `Cmd+Shift+Z`（ADR-0017）。
    Undo,
    Redo,
    /// 自動保存の監視。dirty の間だけ流れてくる。
    Tick(Instant),
    /// キー入力。フォーカスの位置に関係なく全部流れてくる。
    Key(keyboard::Event),
    /// ウィンドウがフォーカスを失った。**修飾キーの押しっぱなしを解くためだけに要る。**
    ///
    /// `ModifiersChanged` はフォーカスを持っている間しか来ない。⌘ を押したまま
    /// `Cmd+Tab` で抜けて向こうで離すと、離した通知が来ずに `modifiers` が
    /// 立ちっぱなしになり、戻ってきたとき入力欄が無反応になる（ADR-0010）。
    WindowUnfocused,
    PaletteQueryChanged(String),
    /// パレットを閉じる。✕ ボタンと背景クリックから飛ぶ。
    PaletteClose,
    /// `Cmd+Shift+V`。エディタと読み取り専用プレビューを行き来する（ADR-0019）。
    TogglePreview,
    /// プレビュー内のリンククリック。既定ブラウザで開く。
    LinkClicked(markdown::Uri),
    /// ノートを新規作成する（`Cmd+N`）。
    NewNote,
    /// リネームを開始する（`Cmd+R`）。入力欄に現在のファイル名を入れて開く。
    RenameStarted,
    RenameChanged(String),
    RenameCancel,
    RenameCommit,
    /// 開いているノートを `.trash` へ退避する（`Cmd+Delete`）。
    DeleteNote,
    /// 保存が終わった（`app.saving` の 1 本に対応する）。
    ///
    /// 中身が結果だけなのは、**同時に 1 本しか走らせない**から。どの保存の完了かは
    /// `app.saving` を見れば一意に決まる（`InFlight` の doc 参照）。
    /// `std::io::Error` は `Clone` でないので、ここへ載せる前に文字列にする。
    Saved(Result<(), String>),
    /// 初回セットアップ: 提示された候補をそのまま採用する（ADR-0015）。
    SetupUseSuggested,
    /// 初回セットアップ: OS のフォルダ選択を開く。
    ///
    /// **これは UI の都合ではなく許可を取る手続き**でもある。ユーザーが選んだ場所は
    /// macOS が暗黙に許可する（ADR-0015）。
    SetupBrowse,
    /// 初回セットアップ: フォルダ選択の結果。`None` は取り消し（何もしない）。
    SetupPicked(Option<PathBuf>),
    /// ウィンドウを閉じる要求。
    ///
    /// **3 つの終了操作が、それぞれ違う道でここへ来る**（ADR-0021）:
    ///
    /// - **✕ ボタン** … `performClose:` → `windowShouldClose:` → winit の `CloseRequested`
    /// - **`Cmd+Q`** … 既定メニューの Quit 項目。素のままだと `terminate:` で
    ///   **プロセスを即死させる**ので、起動時に飛び先を `performClose:` へ付け替えて
    ///   ✕ と同じ道に合流させている（`Message::RetargetQuitMenu`）
    /// - **`Cmd+W`** … メニューに Close 項目が無く、届け先を持たない普通のキーイベント。
    ///   `map_event` が拾ってここへ写す
    ///
    /// **既定の `exit_on_close_request = true` のままだと、これを受け取る前にプロセスが
    /// 終わる。** デバウンス（1 秒）の途中で閉じれば、その分の編集はディスクにも
    /// メモリにも残らず消える。`main` で false にして、保存を挟めるようにしてある。
    CloseRequested(iced::window::Id),
    /// 起動直後に 1 回だけ流れる。Quit 項目の飛び先を付け替える（ADR-0021）。
    ///
    /// **boot からしか流さない。** これを受けたときだけ AppKit に触るので、
    /// テストが送らない限り `update()` は今までどおり純粋に動く。
    RetargetQuitMenu,
}

/// **いつディスクへ書くかを決める、この機能の心臓部。**
///
/// `update()` に時間比較を直接埋めると、境界を確かめるのに実時間を待つしかなくなる。
/// 純粋関数に切り出してあるので、`Instant` を組み立てるだけで境界値を全部テストできる。
///
/// 判定は 2 本の OR:
///
/// - **デバウンス**: 最後の打鍵から `AUTOSAVE_DEBOUNCE` 経過（手を止めたら書く）
/// - **上限**: dirty になってから `AUTOSAVE_MAX_WAIT` 経過（打ち続けていても書く）
///
/// 上限が無いと「打鍵が止まらない限り永久に保存されない」穴が開く。そして上限が効くのは
/// `dirty_since` が **false → true の遷移でだけ**記録されている場合に限る。
///
/// この 2 本に**バックオフの AND** を掛ける。保存が失敗すると `dirty_since` も `last_edit` も
/// 動かないので、この 2 本は以後**永久に真**のままになる。`retry_after` で塞がないと、
/// 100ms の tick ごとに UI スレッドで「一時ファイル作成 → 書き込み → `sync_all`」を
/// 再試行し続け、**保存が遅い・できない環境ほど画面が固まる**（ADR-0012）。
fn should_save(
    now: Instant,
    last_edit: Instant,
    dirty_since: Instant,
    retry_after: Option<Instant>,
) -> bool {
    let due = now.duration_since(last_edit) >= AUTOSAVE_DEBOUNCE
        || now.duration_since(dirty_since) >= AUTOSAVE_MAX_WAIT;
    due && retry_after.is_none_or(|at| now >= at)
}

/// 連続失敗回数から次の再試行までの待ち時間を出す。1 回目の失敗が `failures == 1`。
///
/// 500ms から倍々にして 30 秒で頭打ち。**この曲線は「どれだけ早く直ったことに気づくか」と
/// 「壊れている間どれだけ UI を止めないか」の綱引き**で、速いほうへ倒せば失敗のたびに
/// 同期 I/O が UI スレッドへ戻ってくる。
fn save_retry_delay(failures: u32) -> Duration {
    let steps = failures.saturating_sub(1).min(6);
    SAVE_RETRY_MIN
        .saturating_mul(1 << steps)
        .min(SAVE_RETRY_MAX)
}

/// 等幅フォント。**`Font::MONOSPACE` は使わない**（漢字が消える。ADR-0003）。
///
/// 「等幅なら何でもいい」を意味する `Font::MONOSPACE` を渡すと、cosmic-text が漢字に
/// macOS の `GB18030 Bitmap` を選び、Swash がラスタライズに失敗してグリフごと捨てる。
/// 等幅は必ず**名指し**する。
///
/// **`"Osaka-Mono"` から乗り換えた**（ADR-0003 追記 2）。あれは PostScript 名で、fontdb が
/// 照合するファミリ名ではない。しかもファミリ名 `"Osaka"` には比例の `Osaka.ttf` と等幅の
/// `OsakaMono.ttf` が**同じ weight・同じ幅で同居している**ので、`"Osaka"` に直しても
/// 等幅 face を選べる保証がない。**名前で face を選び分けられないフォントは名指しの対象外**。
///
/// `BIZ UDGothic` を選んだ理由は 3 つとも実測できる:
///
/// - ファミリ名が一意（比例版は `BIZ UDPGothic` という**別ファミリ**）
/// - Regular が `usWeightClass = 400` ちょうど。cosmic-text は weight 完全一致でしか
///   名指しファミリを採らない（`font_weight_diff == 0`。ADR-0009 の帰結）
/// - ASCII が 1024、漢字・かな・約物が 2048（upem 2048）。**ちょうど 1:2**
const EDITOR_FONT: Font = Font::with_name("BIZ UDGothic");

/// 見出し用の明朝。ADR-0003 と同じく**名指しでバンドルしない**。
///
/// 総称ファミリではなく名指しなので、漢字が消える方向の事故は起きない。
/// このフォントが無い環境では cosmic-text が既定フォントへ**静かに**落ちる
/// （見出しがゴシックになるだけで、文字は欠けない）。
///
/// **weight は `Light`（300）を名指しする。** cosmic-text 0.15 は、名指しした
/// ファミリに**要求と同じ weight の face があるときだけ**そのファミリを使う
/// （`FontFallbackIter::default_font_match_key` が `font_weight_diff == 0` で絞る）。
/// Hiragino Mincho ProN は W3（300）と W6（600）しか持たないので、既定の
/// `Normal`（400）で要求すると**フォントが入っていても**静かにゴシックへ落ちる。
const HEADING_FONT: Font = Font {
    weight: iced::font::Weight::Light,
    ..Font::with_name("Hiragino Mincho ProN")
};

/// フォルダごとの件数を数えて多い順に並べる。
///
/// **サブフォルダが 1 つも無い vault では `/` の行を出さない。** ルート直下しか無ければ
/// `/` の件数は「すべて」と必ず一致し、選んでも絞り込みが起きない行になる。
/// フォルダを作る導線がアプリに無い（Finder の仕事）ので、この状態はしばらく続く。
fn count_folders(notes: &[vault::Note]) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for note in notes {
        *counts.entry(note.folder.as_str()).or_insert(0) += 1;
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(name, count)| (name.to_string(), count))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if out.len() == 1 && out[0].0 == vault::ROOT_FOLDER {
        return Vec::new();
    }
    out
}

/// `notes` を触ったあとに、そこから導かれる状態（フォルダ件数・一覧）を作り直す。
///
/// **`notes` を変更したら必ずここを通すこと。** 派生状態が 2 つあると片方だけ更新する
/// コードが書けてしまい、「新規作成してもサイドバーの件数が増えない」という食い違いになる
/// （実際に踏んだ）。約 1200 件を数え直すが、作成・削除は打鍵と違ってホットパスではない。
fn refresh_derived(app: &mut App) {
    app.folders = count_folders(&app.notes);
    app.visible = visible_indices(&app.notes, app.selected_folder.as_deref());
}

/// 絞り込むフォルダを変える。**選択と一覧をここでしか動かさない。**
/// 2 行 1 組を呼び出し側に書かせると、片方だけ更新するコードが書ける。
fn set_folder(app: &mut App, folder: Option<String>) {
    app.selected_folder = folder;
    app.visible = visible_indices(&app.notes, app.selected_folder.as_deref());
}

/// 一覧に出すノートの index を作り直す。フォルダ選択が変わった時だけ呼ぶ。
fn visible_indices(notes: &[vault::Note], folder: Option<&str>) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, note)| folder.is_none_or(|f| note.folder == f))
        .map(|(i, _)| i)
        .collect()
}

/// ノートが 1 件も無いときに出す案内（ADR-0022）。**エディタの中身ではない** —
/// 未選択のときはエディタそのものを画面に出さない（`empty_pane`）。
const EMPTY_VAULT: &str = "ノートがまだありません。Cmd+N で最初のノートを作ります。";

/// `$HOME`。**`.app` から起動しても launchd が渡すので、ここは環境変数で足りる**
/// （シェルの設定に依存する `VAULT` とは事情が違う）。
fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// 読み込みで欠けたものを 1 行にする。欠けていなければ `None`。
///
/// **件数だけでは打つ手が分からない**ので、1 件目の理由まで出す（`vault::Load` の doc 参照）。
/// `.app` の利用者に stderr は見えないので、ここが唯一の通知経路になる。
fn load_warning(load: &vault::Load) -> Option<String> {
    let first = load.first_failure.as_deref()?;
    Some(match load.failed {
        1 => format!("1 件読めませんでした（{first}）"),
        n => format!("{n} 件読めませんでした（例: {first}）"),
    })
}

/// 常駐エラーは 1 行しか出せないので、同時に出る用があれば繋げる。
/// （`refuse_or_rescue` が「／」で足すのと同じ形にしてある）
fn join_errors(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}／{b}")),
        (a, b) => a.or(b),
    }
}

fn boot(root: PathBuf, load: vault::Load, load_ms: f64) -> App {
    let folders = count_folders(&load.notes);
    let visible = visible_indices(&load.notes, None);
    let error = load_warning(&load);
    let notes = load.notes;

    let mut app = App {
        root,
        setup: None,
        config_file: config::config_file(&home_dir()),
        notes,
        folders,
        selected_folder: None,
        visible,
        selected: None,
        content: text_editor::Content::new(),
        palette: None,
        rename: None,
        preview: None,
        modifiers: keyboard::Modifiers::empty(),
        dirty_since: None,
        last_edit: Instant::now(),
        show_marker: false,
        error,
        undo: Vec::new(),
        redo: Vec::new(),
        edit_group: None,
        close_refused: false,
        save_failures: 0,
        retry_after: None,
        saved_flash_until: None,
        saves: 0,
        load_ms,
        last_view_us: Cell::new(0),
        saving: None,
        pending: None,
    };

    // 読み込み直後から「開いているノートがある」状態にする（ADR-0022）。
    open_newest(&mut app);
    app
}

/// 保存先が決まっていないときの起動（ADR-0015）。
///
/// `root` には候補を入れておくが、**`setup` が `Some` の間は誰もそこへ書かない**
/// （`update()` の入口で他のメッセージを落とす）。
fn boot_setup(suggested: PathBuf) -> App {
    let mut app = boot(suggested.clone(), vault::Load::default(), 0.0);
    app.setup = Some(Setup {
        suggested,
        error: None,
    });
    app
}

/// 起動時に保存先を決める（ADR-0015）。**順番そのものが仕様**なので、ここだけ見れば分かる形にする。
///
/// 1. `VAULT` 環境変数 — 開発用。**指定が外れていたら代わりを探さずに終わる**
///    （借り物 vault を指したつもりで別の場所を開き、そこへ書き始めるのを防ぐ。ADR-0002）
/// 2. 記憶した場所 — 前回選んだ場所。**消えていたらセットアップへ戻す**
///    （既定パスへ黙って倒すと、同期の失敗などで vault が見えないときに「空になった」と誤解する）
/// 3. どちらも無ければ初回セットアップ
///
/// `remembered` を引数で受け取るのは、設定ファイルの読み方をここに混ぜないため
/// （読むのは `config::read_vault`、判断するのはここ）。
fn resolve_vault(
    env_vault: Option<String>,
    remembered: Option<PathBuf>,
    suggested: PathBuf,
) -> VaultChoice {
    if let Some(raw) = env_vault {
        let root = PathBuf::from(raw);
        return if root.is_dir() {
            VaultChoice::Ready(root)
        } else {
            VaultChoice::Refuse(format!("VAULT が指す vault が見つかりません: {}", root.display()))
        };
    }

    match remembered {
        Some(root) if root.is_dir() => VaultChoice::Ready(root),
        // 記憶はあるが指す先が無い。**エラーで終わらせない**（アプリから選び直せる）。
        _ => VaultChoice::NeedsSetup { suggested },
    }
}

/// セットアップで決まった場所を採用する。**ここで初めてディスクに触る**（ADR-0015）。
///
/// 失敗したらセットアップ画面に留まって理由を出す。**空の vault を開いて先へ進まない。**
fn adopt_vault(app: &mut App, root: PathBuf) {
    if let Err(e) = std::fs::create_dir_all(&root) {
        set_setup_error(
            app,
            format!("保存先を用意できませんでした（{}）: {e}", root.display()),
        );
        return;
    }

    // 記憶できなくても開くのは続ける（次回またこの画面が出るだけで、書いたものは失わない）。
    // ただし黙らない — 下で常駐エラーに出す。
    let remembered = config::write_vault(&app.config_file, &root);

    let t0 = Instant::now();
    let load = vault::load_dir(&root);
    app.load_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // `notes` を move する前に読む。
    let warning = load_warning(&load);

    app.root = root;
    app.selected_folder = None;
    app.notes = load.notes;
    refresh_derived(app);
    // 起動時と同じ規則で開く（ADR-0022）。
    open_newest(app);
    app.setup = None;
    // **読めなかった件数も、記憶できなかった件も、どちらも黙らない。**
    // 常駐エラーは 1 行なので繋げて出す（片方だけ出すと、もう片方が消える）。
    app.error = join_errors(
        remembered
            .err()
            .map(|e| format!("保存先を覚えられませんでした（次回もこの画面が出ます）: {e}")),
        warning,
    );
}

fn set_setup_error(app: &mut App, message: String) {
    if let Some(setup) = &mut app.setup {
        setup.error = Some(message);
    }
}

/// 未保存の編集があるか。**`dirty_since` が唯一の真実**で、bool は持たない。
fn is_dirty(app: &App) -> bool {
    app.dirty_since.is_some()
}

/// 保存が済んだ（または差分が無かった）ときに dirty 関連を畳む。
fn clear_dirty(app: &mut App) {
    app.dirty_since = None;
    app.show_marker = false;
    clear_save_backoff(app);
}

/// **ディスクが健康だと分かったとき**に畳むもの。dirty とは分けてある。
///
/// 保存中にも打鍵は続けられるので、「書けたが、書いた時点より新しい本文がある」状態が起きる。
/// そのとき dirty は立てたままにしないと打った分が宙に浮くが、**書けた事実は事実**なので
/// バックオフと「閉じるのを断った記憶」は解いてよい。
fn clear_save_backoff(app: &mut App) {
    // 書けたなら、閉じるのを断った記憶も畳む。次に閉じられなくなったときは
    // また警告から始める（`close_refused` の doc 参照）。
    app.close_refused = false;
    // バックオフも畳む。**ここが唯一の解除点**なので、ユーザー操作（切替・終了）で
    // 保存が通ったときも自動保存の待ちが解ける。
    app.save_failures = 0;
    app.retry_after = None;
}

/// 選択中のノートの書き戻しを**投げる**。`next` は保存が通ったあとにやること。
///
/// **旧 `save_now()` の置き換え**（ADR-0014）。あれは `bool` を返す同期関数で、
/// 呼び出し元は `if !save_now(app) { return; }` でエディタを潰す操作を止めていた。
/// 書き込み（`write` → `sync_all` → `rename`）を UI スレッドで待つので、
/// 遅い vault では `sync_all` が返るまで**画面全体が止まる**。
///
/// ガードの意味論は変えていない。**判定の場所だけが `Message::Saved` へ移った**:
///
/// - 書くものが無い（未選択・差分なし）→ `next` を**その場で**実行する。ここは以前と同じ
/// - 書くものがある → `next` を `app.pending` に預け、書き込みだけワーカーへ出す
/// - 既に 1 本走っている → 走らせない。`next` だけ預けて完了時に引き継ぐ
///
/// `autosave_at` は自動保存の tick から来た時刻。ユーザー操作なら `None`
/// （バックオフを数えるのは自動保存だけ。ADR-0012）。
fn begin_save(
    app: &mut App,
    next: Option<PendingAction>,
    autosave_at: Option<Instant>,
) -> Task<Message> {
    // **預けるのは `Some` のときだけ。** 自動保存の tick（`next` が `None`）が
    // 先に預けてある操作を消してはいけない。
    if let Some(next) = next {
        app.pending = Some(next);
    }

    // 走っている最中に重ねない。完了時に `app.pending` ごと引き継がれる。
    if app.saving.is_some() {
        return Task::none();
    }

    let Some(index) = app.selected else {
        return take_pending(app);
    };

    let contents = app.content.text();

    // 内容が変わっていないなら書かない。カーソルを動かしただけ・開いただけで mtime が動くと、
    // 外部ツール（git・エディタ・同期）から「更新された」と誤認される。
    if contents == app.notes[index].raw {
        clear_dirty(app);
        return take_pending(app);
    }

    let path = app.notes[index].path.clone();
    app.saving = Some(InFlight {
        index,
        path: path.clone(),
        contents: contents.clone(),
        autosave_at,
    });

    // **`spawn_blocking` で回す。** `vault::save` は `sync_all` でディスクを待つ本物の
    // ブロッキング I/O なので、そのまま async へ置くと tokio のワーカーを 1 本占有する。
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                vault::save(&path, &contents).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(format!("保存タスクが落ちました: {e}")))
        },
        Message::Saved,
    )
}

/// 預けてある操作があれば実行する。無ければ何もしない。
fn take_pending(app: &mut App) -> Task<Message> {
    match app.pending.take() {
        Some(action) => run_action(app, action),
        None => Task::none(),
    }
}

/// 保存が通ったので、待たせていた操作を実行する。
///
/// **ここへ来る時点で本文はディスクに乗っている。** どれもエディタの内容を捨てるが、
/// 捨ててよいことが保証された後だけ呼ばれる。
fn run_action(app: &mut App, action: PendingAction) -> Task<Message> {
    match action {
        PendingAction::OpenNote { index, from_palette } => {
            if from_palette {
                app.palette = None;
                // 検索は全ノートが対象なので、別フォルダのノートが当たる。そのまま開くと
                // 「選択中のノートが左の一覧に無い」状態になるため、そのときだけ絞り込みを解除する。
                //
                // **絞り込みの中のノートなら何もしない。** 以前はここで無条件に解除していて、
                // 「topics で絞り込んで topics のノートを開いたのに、すべてに戻る」動きになっていた。
                // 守りたいのは「開いたノートが一覧に見える」ことであって、解除そのものではない。
                let hidden_by_filter = app
                    .selected_folder
                    .as_deref()
                    .is_some_and(|folder| folder != app.notes[index].folder);
                if hidden_by_filter {
                    set_folder(app, None);
                }
            }
            open_note(app, index);
            Task::none()
        }
        PendingAction::NewNote => {
            // インデックスがずれるので、古い `matches` を持ったパレットは畳む。
            app.palette = None;
            match create_note(app) {
                Ok(index) => {
                    open_note(app, index);
                    // 作った直後に打ち始められるようにフォーカスを移す。
                    iced::widget::operation::focus(iced::widget::Id::new(EDITOR_ID))
                }
                Err(e) => {
                    app.error = Some(e);
                    Task::none()
                }
            }
        }
        PendingAction::Rename(input) => {
            match commit_rename(app, &input) {
                Ok(()) => {
                    app.rename = None;
                    app.error = None;
                }
                // **入力欄は開いたままにする。** 閉じてしまうと打ち直せない。
                Err(e) => app.error = Some(e),
            }
            Task::none()
        }
        PendingAction::Delete => {
            if let Err(e) = delete_note(app) {
                app.error = Some(e);
            }
            Task::none()
        }
        PendingAction::Close(id) => iced::window::close(id),
    }
}

/// 保存の完了を受け取る。**ガードの判定はここでやる**（旧 `save_now()` の戻り値の役目）。
fn finish_save(app: &mut App, result: Result<(), String>) -> Task<Message> {
    // 対応する保存が無い完了は捨てる。直列なので通常は起きないが、
    // 起きたときに黙って `notes` を書き換えるほうが危ない。
    let Some(in_flight) = app.saving.take() else {
        return Task::none();
    };

    match result {
        Ok(()) => {
            // ファイルが正になったので、メモリ側のメタ情報も取り直す。タイトル行を編集したら
            // 一覧にすぐ反映されてほしい。**ここで並べ替えはしない**（`notes` の doc 参照）。
            //
            // mtime は書いた直後の now でよい。次回起動時にディスクから読み直される値であって、
            // ここで 1 回 stat を打ち直すほどの精度は要らない。
            let modified = std::time::SystemTime::now();
            let up_to_date = app.content.text() == in_flight.contents;
            app.notes[in_flight.index] = vault::parse_note(
                &app.root,
                in_flight.path,
                in_flight.contents,
                modified,
            );
            app.error = None;
            app.saves += 1;
            app.saved_flash_until = Some(Instant::now() + SAVED_FLASH);

            // **保存中に打たれた分があれば dirty を畳まない。** 畳むと、書き出した
            // スナップショットより新しい本文が「保存済み」に見えて、次の切替で消える。
            if up_to_date {
                clear_dirty(app);
            } else {
                clear_save_backoff(app);
            }

            // 預けてある操作は `begin_save` へ返す。まだ書けていない分があれば
            // もう一度書いてから実行され、無ければその場で実行される。
            if app.pending.is_some() {
                return begin_save(app, None, None);
            }
            Task::none()
        }
        Err(e) => {
            // dirty は立てたままにする。表示が消えないことが異常の合図。
            app.error = Some(format!("保存に失敗: {e}"));
            if let Some(at) = in_flight.autosave_at {
                app.save_failures = app.save_failures.saturating_add(1);
                app.retry_after = Some(at + save_retry_delay(app.save_failures));
            }
            // **待たせていた操作は捨てる。これが旧 `if !save_now()` の中身。**
            // 進めてしまうと、書けなかった本文はディスクにもメモリにも残らない。
            match app.pending.take() {
                Some(PendingAction::Close(id)) => refuse_or_rescue(app, id),
                _ => Task::none(),
            }
        }
    }
}

/// 元に戻せる状態（ADR-0017）。**カーソルまで戻す**ので位置は `Cursor` で持つ。
///
/// `Cursor` は行・列の論理位置（描画上の座標ではない）なので、本文を作り直したあとでも
/// `Content::move_to` でそのまま復元できる。
#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    cursor: iced::advanced::text::editor::Cursor,
}

/// 編集の種類。**同じ種類が続く間は 1 つの undo ステップにまとめる**（ADR-0017）。
///
/// 1 打鍵 1 ステップだと「あいう」を消すのに 3 回押すことになり、実用にならない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    /// 改行は入力と分けて数える。段落ごとに戻せるほうが目的の状態へ着きやすい。
    Enter,
    Backspace,
    Delete,
    /// 貼り付けは 1 回で 1 ステップ。連続しても混ぜない意味はないのでまとめる。
    Paste,
    /// インデント操作（このアプリからは出ないが、`Edit` の網羅性のために持つ）。
    Indent,
}

/// undo に積める段数の上限。
///
/// 全文スナップショットなので、無制限だと大きなノートを長時間編集したときに
/// 「本文の長さ × 打鍵の区切り数」だけ積み上がる。100 段あれば実用上は足り、
/// 100KB のノートでも 10MB で頭打ちになる。
const UNDO_LIMIT: usize = 100;

/// いまのエディタの状態を写し取る。
fn snapshot(app: &App) -> Snapshot {
    Snapshot {
        text: app.content.text(),
        cursor: app.content.cursor(),
    }
}

/// 写し取った状態へ戻す。
fn restore(app: &mut App, snapshot: Snapshot) {
    app.content = text_editor::Content::with_text(&snapshot.text);
    app.content.move_to(snapshot.cursor);
    // 戻した直後の編集は、必ず新しいまとまりから始める。
    app.edit_group = None;
}

/// エディタの中身を丸ごと差し替える。**undo の履歴もここで畳む。**
///
/// **履歴はノート単位のもの**なので、本文の差し替えと同時に捨てないと、
/// **開いていないノートの本文が今のノートへ書き込まれる**。差し替えの入口を
/// この関数 1 つに絞ってあるのは、その漏れを構造で防ぐため（ADR-0017）。
fn replace_content(app: &mut App, text: &str) {
    app.content = text_editor::Content::with_text(text);
    // プレビュー中のノート切替はプレビューのまま読み続ける（ADR-0019）。表示は常に
    // 本文の写しなので、差し替えたらここで一緒に作り直す。ここは `selected` が動く
    // 全経路（一覧クリック・パレット・新規作成・削除）の合流点で、畳み忘れが起きない。
    if app.preview.is_some() {
        app.preview = Some(markdown::Content::parse(text));
    }
    app.undo.clear();
    app.redo.clear();
    app.edit_group = None;
}

/// 編集が起きたことを dirty へ記録する。**`Message::Edit` と undo / redo で共有する。**
fn mark_edited(app: &mut App) {
    if app.selected.is_some() {
        app.last_edit = Instant::now();
        // **false → true の遷移でだけ**記録する。
        app.dirty_since.get_or_insert_with(Instant::now);
    }
}

/// undo の区切りを判定し、必要なら**編集が起きる前**の状態を積む（ADR-0017）。
fn remember_for_undo(app: &mut App, action: &text_editor::Action) {
    use iced::advanced::text::editor::{Action, Edit};

    let kind = match action {
        Action::Edit(edit) => match edit {
            Edit::Insert(_) => EditKind::Insert,
            Edit::Enter => EditKind::Enter,
            Edit::Backspace => EditKind::Backspace,
            Edit::Delete => EditKind::Delete,
            Edit::Paste(_) => EditKind::Paste,
            Edit::Indent | Edit::Unindent => EditKind::Indent,
        },
        // スクロールは本文にもカーソルにも触らないので、まとまりを切らない。
        Action::Scroll { .. } => return,
        // **カーソルが動いたら次の編集は別のまとまり。** ここを切らないと、
        // 行頭で打った文字と行末で打った文字が 1 回の undo でまとめて消える。
        _ => {
            app.edit_group = None;
            return;
        }
    };

    if app.edit_group == Some(kind) {
        return;
    }
    app.edit_group = Some(kind);

    // **新しい編集は redo の先を捨てる。** 戻したあとに別の編集をしたら、
    // やり直せるはずだった歴史はもう繋がらない。
    app.redo.clear();
    app.undo.push(snapshot(app));
    if app.undo.len() > UNDO_LIMIT {
        app.undo.remove(0);
    }
}

fn open_note(app: &mut App, index: usize) {
    // frontmatter 込みの全文をエディタへ渡す。`.md` が唯一の真実なので、
    // 表示のために本文を加工しない。
    //
    // **一度クローンしてから渡す。** `replace_content` は `&mut App` を取るので
    // `app.notes` を借りたままでは呼べない。切替のたびに数 KB 余分に写すが、
    // これはユーザー操作のたびに 1 回で、打鍵ごとに走る経路ではない。
    let Some(raw) = app.notes.get(index).map(|note| note.raw.clone()) else {
        return;
    };
    // **`replace_content` を通す。** undo の履歴を畳まないと、切り替えたあとの
    // `Cmd+Z` が**前のノートの本文**をこのノートへ書き込む（ADR-0017）。
    replace_content(app, &raw);
    app.selected = Some(index);
    // 開きかけのリネーム欄は、選択が動いた時点で対象を失うので畳む。
    // ここは `selected` を書き換える全経路（一覧クリック・パレット・新規作成）の
    // 合流点。畳み忘れると、残った入力欄の Enter が**新しい選択先を前のノートの
    // 名前でリネームする**（`RenameCommit` の対象は `app.selected`）。
    app.rename = None;
}

/// 読み込み直後に「開いているノートが必ずある」状態にする（ADR-0022）。
///
/// 一覧は `vault::sort_notes` が mtime 降順で固定しているので、**先頭が直近に更新された
/// ノート**。ここは選択を動かすだけで、`begin_save` は `contents == raw` で抜けるため
/// **ディスクには書かない**（無編集で mtime を動かさない規律）。
///
/// 空 vault のときだけ未選択のままにする。そのときエディタは画面に出ない（`empty_pane`）。
fn open_newest(app: &mut App) {
    if app.notes.is_empty() {
        app.selected = None;
        replace_content(app, "");
        return;
    }
    open_note(app, 0);
}

/// 新規ノートを作って `notes` の先頭に差し込む。返り値は挿入した位置（常に 0）。
///
/// 置き場所は**いま絞り込んでいるフォルダ**。絞り込んでいなければ開いているノートと
/// 同じフォルダ、それも無ければ vault ルート。
///
/// 順序が逆だと（開いているノートを優先すると）、`topics` のノートを開いたまま `notes` で
/// 絞り込んで `Cmd+N` したときに `topics` へ作られ、それを見せるために絞り込みが解除される。
/// **見ているフォルダに増える**ほうが期待に近い（実際に触って分かった）。
///
/// 先頭に差し込むのは、一覧が読み込み時の順序（更新日時の新しい順）で固定されているから。
/// 末尾に足すと 約 1200 件スクロールした先に出て「作ったのに見えない」になる。
fn create_note(app: &mut App) -> Result<usize, String> {
    let dir = app
        .selected_folder
        .as_ref()
        .map(|folder| app.root.join(folder))
        .or_else(|| {
            app.selected
                .and_then(|i| app.notes[i].path.parent().map(|p| p.to_path_buf()))
        })
        .unwrap_or_else(|| app.root.clone());

    let name = format!("{}.md", chrono::Local::now().format(NEW_NOTE_NAME_FORMAT));
    // `reserve_unique` は `create_new` で空ファイルを確保するので、同一秒に連打しても
    // 既存を上書きしない（`-2`, `-3` と枝番が付く）。
    let path = vault::reserve_unique(&dir, &name).map_err(|e| format!("作成に失敗: {e}"))?;
    let note = vault::parse_note(&app.root, path, String::new(), std::time::SystemTime::now());

    // **先頭への差し込みで既存のインデックスが全部 1 つずれる。** ここを忘れると
    // 「作成した瞬間に、開いていたノートが隣のノートにすり替わる」バグになる。
    app.notes.insert(0, note);
    if let Some(selected) = app.selected {
        app.selected = Some(selected + 1);
    }

    // 作ったノートが絞り込みの外に出るなら、絞り込みを解除する。
    // 「作成したノートは必ず一覧に見える」を保つための最後の砦（パレットと同じ規則）。
    // 絞り込み中は必ずそのフォルダに作るので通常は発火しない。フォルダ名とパスの対応が
    // 崩れたとき（vault の外を指す絞り込み等）に、見えないノートを作らないための保険。
    if app
        .selected_folder
        .as_deref()
        .is_some_and(|folder| folder != app.notes[0].folder)
    {
        app.selected_folder = None;
    }
    // フォルダ件数も作り直す。忘れるとサイドバーの数字が増えない。
    refresh_derived(app);

    Ok(0)
}

/// リネーム先の名前を検証して、実際に使うファイル名へ整える。
///
/// **拒否する理由はどれも「一覧から消えるから」に集約される。**
///
/// - **dot 始まり**: `load_dir()` が dot 始まりを走査から除外する。`.secret` に改名すると
///   再起動後に一覧から消える（前身で実際に踏んだ）
/// - **`/` `\` を含む**: 別ディレクトリへ移動してしまう。リネームは名前を変える操作であって
///   移動ではない
/// - **空**: ファイル名にならない
///
/// 拡張子は補う。`load_dir()` は `.md` しか拾わないので、`設計メモ` のまま保存すると
/// dot 始まりと**同じ理由で**一覧から消える。
fn validate_note_name(input: &str) -> Result<String, String> {
    let name = input.trim();
    if name.is_empty() {
        return Err("名前が空です".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("名前に / や \\ は使えません".to_string());
    }
    if name.starts_with('.') {
        return Err(". で始まる名前は一覧から消えるため使えません".to_string());
    }
    Ok(if name.ends_with(".md") {
        name.to_string()
    } else {
        format!("{name}.md")
    })
}

/// リネームを確定する。**ファイル名だけを変える**（本文には触らない）。
///
/// 表示タイトルは frontmatter の `title:` > 本文の `# 見出し` > ファイル名の stem で決まるので、
/// frontmatter を持つノートは**リネームしても一覧の見た目が変わらない**。仕様どおり。
fn commit_rename(app: &mut App, input: &str) -> Result<(), String> {
    let Some(index) = app.selected else {
        return Err("ノートが開かれていません".to_string());
    };
    let name = validate_note_name(input)?;

    let path = app.notes[index].path.clone();
    let dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| app.root.clone());
    if path.file_name().is_some_and(|current| current == name.as_str()) {
        return Ok(()); // 同じ名前。書き込みも枝番も起こさない
    }

    // 衝突は枝番で避ける（`move_to` が `reserve_unique` 経由で予約してから rename する）。
    let dest = vault::move_to(&path, &dir, &name).map_err(|e| format!("リネームに失敗: {e}"))?;

    // ファイル名が変わるとタイトルが変わり得る（frontmatter も見出しも無いノート）。
    // 本文と mtime はそのまま持ち越す。rename は中身にも mtime にも触らない。
    let raw = app.notes[index].raw.clone();
    let modified = app.notes[index].modified;
    app.notes[index] = vault::parse_note(&app.root, dest, raw, modified);
    refresh_derived(app);
    Ok(())
}

/// 開いているノートを `.trash` へ退避する。
///
/// **確認ダイアログは出さない。** vault 内 `.trash/` への移動で取り消せる操作なので、
/// 確認を挟むほうが邪魔になる（前身と同じ判断）。
///
/// **書き戻しは呼び出し元（`Message::DeleteNote`）でやる。** 確認を出さない以上、
/// 取り消しの綱は `.trash` のファイルだけ。そこに「最後に自動保存された内容」しか
/// 残らないと、直前に打った分は誤って消した瞬間に取り返せない。
///
/// 以前は「保存に失敗したときに削除できなくなるほうが困る」として書き戻していなかったが、
/// **その心配は成立しない。** `.trash` への退避は同じディレクトリからの rename なので、
/// 保存が権限で失敗する状況では削除もどのみち失敗する。守れるものが増えるだけ。
fn delete_note(app: &mut App) -> Result<(), String> {
    let Some(index) = app.selected else {
        return Err("ノートが開かれていません".to_string());
    };

    let path = app.notes[index].path.clone();
    vault::move_to_trash(&app.root, &path).map_err(|e| format!("削除に失敗: {e}"))?;

    // 取り除くと後ろのインデックスが繰り上がる。**消した位置に居続ける**ので、
    // 同じ index を開き直す（末尾を消したときだけ 1 つ前）。ADR-0022 の
    // 「未選択 ⟺ ノートが 0 件」を保つため、最後の 1 件を消したときだけ選択を外す。
    //
    // どちらの枝も `replace_content` を通る。消したノートの履歴を残すと、
    // 次に開いたノートで `Cmd+Z` が消したはずの本文を蘇らせる。
    app.notes.remove(index);
    if app.notes.is_empty() {
        app.selected = None;
        replace_content(app, "");
    } else {
        open_note(app, index.min(app.notes.len() - 1));
    }
    clear_dirty(app);
    refresh_derived(app);
    Ok(())
}

/// 閉じる要求を受けたときに、書き戻してから閉じるか、踏みとどまるかを決める。
///
/// **これが最後のデータ喪失の穴だった。** 一覧の切替・作成・リネーム・削除には
/// すべて `save_now` のガードが入っていたのに、「閉じる」だけが素通りしていた。
/// デバウンス（1 秒）の途中で `Cmd+W` を打てば、その 1 秒分は消える。
///
/// 保存が通ったなら本文は全部ディスクに乗っているので、迷わず閉じてよい
/// （`PendingAction::Close` がそのまま `window::close` になる）。
/// 判断が要るのは**書けなかった**ときで、そこは 2 段階にしてある（`refuse_or_rescue`）。
///
/// - **1 回目**: 閉じない。理由をステータス行に出す（気づく機会を作り、手で退避もできる）
/// - **2 回目**: 明確な意思表示とみなし、**本文を `.rescue` へ退避してから**閉じる。
///   ただし**退避にも失敗したら閉じない**（`rescue_and_close`。ADR-0012）
///
/// **「閉じない」だけで通さなかった理由。** メモ帳で保存が失敗する現実的な原因は
/// 容量不足・ドライブが外れた・権限が変わった、のどれかで、**待っても直らない**。
/// 踏みとどまり続けても再試行が失敗するだけで、出口は強制終了しかなく、結局本文を失う。
/// それは「安全装置が、失敗したときに何が起きるかまで設計されていない」状態そのもの。
///
/// 逆に 1 回目で退避して閉じると、ユーザーは**何が起きたか知らないまま**終了する。
/// だから警告を 1 回挟む。
fn close_window(app: &mut App, id: iced::window::Id) -> Task<Message> {
    begin_save(app, Some(PendingAction::Close(id)), None)
}

/// 保存に失敗した状態で閉じる要求を受けたときの 2 段階（`close_window` の doc 参照）。
fn refuse_or_rescue(app: &mut App, id: iced::window::Id) -> Task<Message> {
    if !app.close_refused {
        app.close_refused = true;
        app.show_marker = true;
        // **次に何が起きるかまで伝える。** 「閉じられない」だけだと打つ手が分からない。
        if let Some(error) = &mut app.error {
            error.push_str("／もう一度閉じると、本文を .rescue に退避して終了します");
        }
        return Task::none();
    }

    // ── 2 回目。退避できたら閉じる ──
    let (name, dirs) = rescue_target(app);
    rescue_and_close(app, id, &name, &dirs)
}

/// 退避先の候補を**優先順**で組む。ファイル名も一緒に返す。
///
/// 保存が失敗した原因がノートのあるディレクトリにあるとは限らないし、逆にそこだけの
/// 問題なら vault ルートには書ける。一時ディレクトリは最後の砦（見つけにくいので優先度は最低）。
fn rescue_target(app: &App) -> (String, Vec<PathBuf>) {
    let note = app.selected.map(|i| &app.notes[i]);
    let name = note
        .and_then(|n| n.path.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled.md".to_string());
    let dirs: Vec<PathBuf> = note
        .and_then(|n| n.path.parent().map(|p| p.to_path_buf()))
        .into_iter()
        .chain([app.root.clone(), std::env::temp_dir()])
        .collect();
    (name, dirs)
}

/// 本文を `.rescue` へ退避して閉じる。**退避に失敗したら閉じない**（ADR-0012）。
///
/// ADR-0007 は「2 回目は必ず閉じる」と決めたが、これは**退避が成功する前提**の話だった。
/// 通常保存も退避先も全滅（容量ゼロなど）したときに閉じると、唯一残っていたメモリ上の
/// 本文を、行き先が 1 つも無いまま捨てることになる。**逃げ道を用意する決定が、
/// 逃げ道ごと消える経路を作っていた。**
///
/// 退避できなかったときに残す状態は「本文がエディタに残り、理由が画面に出ている」。
/// ユーザーは選択してコピーで自力退避でき、容量を空けてからもう一度閉じれば退避は通る。
fn rescue_and_close(
    app: &mut App,
    id: iced::window::Id,
    name: &str,
    dirs: &[PathBuf],
) -> Task<Message> {
    // 閉じたあとに画面は無いので、成功を伝える経路は stderr しかない。vault 直下に落ちれば
    // Finder からは見えるし、`.rescue` は `load_dir` が拾わないので一覧は汚れない。
    match vault::write_rescue(dirs, name, &app.content.text()) {
        Ok(path) => {
            eprintln!("haboku: 保存できなかったので退避しました: {}", path.display());
            iced::window::close(id)
        }
        Err(e) => {
            eprintln!("haboku: 退避にも失敗しました（閉じません）: {e}");
            app.error = Some(format!(
                "保存も退避も失敗: {e}／本文を選択してコピーしてください。空き容量を作れば終了できます"
            ));
            app.show_marker = true;
            Task::none()
        }
    }
}

/// クエリで全ノートを絞り込む。**パレットが開いている間ずっとではなく、クエリが変わった時だけ。**
///
/// 絞り込み中のフォルダは無視して**全ノート**を対象にする。`Cmd+P` は「どのフォルダにいても
/// 目的のノートへ飛ぶ」道具なので、いまの絞り込みに引きずられると用を成さない。
/// **タイトルを先に全部出し、余った枠にだけ本文ヒットを足す**（ADR-0018）。
/// タイトルと本文は別種の証拠なので、1 本のスコアに混ぜない。
fn refilter(notes: &[vault::Note], query: &str) -> Vec<Hit> {
    let mut scored: Vec<(usize, fuzzy::Match)> = notes
        .iter()
        .enumerate()
        .filter_map(|(i, note)| fuzzy::match_query(query, &note.title).map(|m| (i, m)))
        .collect();

    // スコアの高い順。同点はタイトル順で安定させる（同じクエリで並びが変わらないように）。
    scored.sort_by(|a, b| {
        b.1.score
            .cmp(&a.1.score)
            .then_with(|| notes[a.0].title.cmp(&notes[b.0].title))
    });

    // **本文を読むかどうかは打ち切る前に決める。** タイトルだけで 50 件埋まるなら
    // 本文は 1 バイトも読まない。空クエリは全件がタイトルに当たるのでここで止まる。
    let searches_body = scored.len() < PALETTE_MAX_RESULTS
        && query.chars().count() >= BODY_MIN_QUERY_CHARS;
    scored.truncate(PALETTE_MAX_RESULTS);
    let mut hits: Vec<Hit> = scored
        .into_iter()
        .map(|(note, m)| Hit {
            note,
            kind: HitKind::Title(m),
        })
        .collect();

    if !searches_body {
        return hits;
    }

    // 同じノートを二度出さないための印。**本文を読むときは打ち切りが起きていない**
    // （`searches_body` は 50 件に満たないときだけ真）ので、打ち切り後の `hits` から作れる。
    let mut title_hit = vec![false; notes.len()];
    for hit in &hits {
        title_hit[hit.note] = true;
    }

    // 本文は一覧と同じ順（更新の新しい順）に見る。**スコアは作らない。**
    // 重み付けの根拠になる実データが無い状態で決めるのは占いなので、
    // 「一覧と同じ順」という説明できる順序に倒す。
    for (i, note) in notes.iter().enumerate() {
        if hits.len() == PALETTE_MAX_RESULTS {
            break;
        }
        if title_hit[i] {
            continue;
        }
        if let Some(range) = fuzzy::contains_query(query, &note.raw) {
            let (snippet, hit) = fuzzy::snippet(&note.raw, range);
            hits.push(Hit {
                note: i,
                kind: HitKind::Body { snippet, hit },
            });
        }
    }
    hits
}

/// パレットが開いている間のキー操作。
///
/// **戻り値は「処理したか」ではなく Task。** Enter がノートを開く前に保存を挟むようになり
/// （ADR-0014）、この関数からも `Task` を返す必要が出た。呼び出し元は元々
/// 「処理したか」を見ていない（パレットが閉じていれば何も起きないだけ）。
fn handle_palette_key(
    app: &mut App,
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
) -> Task<Message> {
    use keyboard::key::Named;

    let Some(palette) = &mut app.palette else {
        return Task::none();
    };
    let len = palette.matches.len();

    match key {
        keyboard::Key::Named(Named::Escape) => {
            app.palette = None;
            Task::none()
        }
        keyboard::Key::Named(Named::Enter) => {
            let Some(index) = palette.matches.get(palette.selected).map(|hit| hit.note) else {
                app.palette = None;
                return Task::none();
            };
            // ここにも保存ガードが要る。パレットからの選択もエディタを上書きする操作。
            // **パレットは閉じない**。閉じてから中断すると、なぜ切り替わらないのかが
            // 分からなくなる（エラーはステータス行に常駐する）。畳むのは
            // `PendingAction::OpenNote` が実際に走るとき。
            begin_save(
                app,
                Some(PendingAction::OpenNote {
                    index,
                    from_palette: true,
                }),
                None,
            )
        }
        // **矢印キーではなく ctrl-n / ctrl-p。** `text_input` が矢印を消費して親に届かない
        // （gpui 版でも同じ回避策が要った）。
        keyboard::Key::Character(c) if c == "n" && modifiers.control() => {
            if len > 0 {
                palette.selected = (palette.selected + 1) % len;
            }
            snap_palette_to_selected(palette.selected, len)
        }
        keyboard::Key::Character(c) if c == "p" && modifiers.control() => {
            if len > 0 {
                palette.selected = (palette.selected + len - 1) % len;
            }
            snap_palette_to_selected(palette.selected, len)
        }
        _ => Task::none(),
    }
}

/// 選択中の候補が見えるところまで、パレットの候補リストをスクロールさせる。
///
/// iced には「選択中の子へ自動スクロールする」仕組みが無い（`snap_to_end` は末尾固定）。
/// `selected` を動かしてもスクロール位置は据え置かれるので、`update()` から明示的に
/// 指示を返さないと**ハイライトだけが可視領域の外へ出ていく**（実際にそうなっていた）。
///
/// 絶対座標ではなく相対オフセット（先頭 0.0 / 末尾 1.0）で出す。行の高さも、
/// リストの高さ（360px）が行数の整数倍かどうかも計算に要らず、
/// 「末尾を選んだら必ず最下部」が定義から保証される。
fn snap_palette_to_selected(selected: usize, len: usize) -> Task<Message> {
    iced::widget::operation::snap_to(
        iced::widget::Id::new(PALETTE_LIST_ID),
        iced::widget::operation::RelativeOffset {
            x: 0.0,
            y: palette_scroll_offset(selected, len),
        },
    )
}

/// 選択位置を相対オフセット（先頭 0.0 / 末尾 1.0）に直す。
///
/// `Task` にすると中身を検査できないので、境界の出る計算だけ関数に分けてある。
fn palette_scroll_offset(selected: usize, len: usize) -> f32 {
    // 1 件以下なら割る相手がいない。スクロールする余地も無いので先頭で足りる。
    if len <= 1 {
        return 0.0;
    }
    selected as f32 / (len - 1) as f32
}

/// 初回セットアップ中の `update()`（ADR-0015）。**扱うのは 3 つだけで、残りは捨てる。**
fn update_setup(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::SetupUseSuggested => {
            if let Some(setup) = &app.setup {
                adopt_vault(app, setup.suggested.clone());
            }
        }
        Message::SetupBrowse => {
            // 候補そのものはまだ存在しないことがあるので、**親フォルダから開く**。
            let start = app.setup.as_ref().and_then(|setup| {
                setup
                    .suggested
                    .parent()
                    .filter(|parent| parent.is_dir())
                    .map(Path::to_path_buf)
            });

            // **blocking 版を使わない。** UI スレッドでモーダルを回すとイベントループが
            // 止まる（iced 公式の editor 例と同じく `AsyncFileDialog` + `Task`）。
            return Task::future(async move {
                let mut dialog = rfd::AsyncFileDialog::new().set_title("メモの保存先フォルダを選ぶ");
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                dialog
                    .pick_folder()
                    .await
                    .map(|handle| handle.path().to_path_buf())
            })
            .map(Message::SetupPicked);
        }
        // `None` は取り消し。**画面を変えない**（勝手に候補を採用しない）。
        Message::SetupPicked(Some(root)) => adopt_vault(app, root),
        Message::SetupPicked(None) => {}
        // **閉じる要求だけは通す。** `exit_on_close_request` を false にしてあるので、
        // ここで落とすと ✕ も `Cmd+Q` も効かない窓になる。保存するものはまだ無いので
        // `close_window`（保存を挟む経路）ではなく素直に閉じる。
        Message::CloseRequested(id) => return iced::window::close(id),
        // 打鍵・tick など。**保存先が決まる前に何も起こさせない。**
        // （`RetargetQuitMenu` は `update()` の入口で処理済みでここへは来ない）
        _ => {}
    }

    Task::none()
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    // **セットアップ中でも通す**（下の `setup` ガードより手前に置く理由）。boot から
    // 1 回しか流れないので、ここで捨てると初回起動の人だけ Cmd+Q が素通りしたままになる。
    // vault には触らないメッセージなので、ADR-0015 の「選ぶ前に書かない」とは競合しない。
    if matches!(message, Message::RetargetQuitMenu) {
        if let Err(reason) = appkit::retarget_quit_to_close() {
            // **黙って劣化させない。** この行が出ているときの Cmd+Q は保存を待たない。
            app.error = join_errors(
                app.error.take(),
                Some(format!("Cmd+Q の保存ガードを取り付けられません（{reason}）")),
            );
        }
        return Task::none();
    }

    // **保存先が決まるまでは他を一切通さない**（ADR-0015）。`root` はまだ候補で、
    // 自動保存の tick や打鍵がここを抜けると「まだ選んでいない場所」へ書きに行く。
    if app.setup.is_some() {
        return update_setup(app, message);
    }

    match message {
        Message::FolderSelected(folder) => set_folder(app, folder),
        Message::NoteSelected(index) => {
            // **保存に失敗したら遷移しない。** ここで進むと未保存の本文が
            // ディスクにもメモリにも残らず消える。前身でデータ喪失を招いた欠陥がこれ。
            // 判定は `finish_save` がやる（ADR-0014）。
            return begin_save(
                app,
                Some(PendingAction::OpenNote {
                    index,
                    from_palette: false,
                }),
                None,
            );
        }
        Message::Edit(action) => {
            // カーソル移動やクリックで dirty を立てない。編集だけを拾う。
            let is_edit = matches!(action, iced::advanced::text::editor::Action::Edit(_));
            // **編集が起きる前に**区切りを判定して積む（ADR-0017）。
            remember_for_undo(app, &action);
            app.content.perform(action);
            if is_edit {
                mark_edited(app);
            }
        }
        // 元に戻す / やり直す（ADR-0017）。**戻した状態は反対側のスタックへ積む。**
        Message::Undo => {
            let Some(previous) = app.undo.pop() else {
                return Task::none();
            };
            app.redo.push(snapshot(app));
            restore(app, previous);
            // 戻した本文もディスクへ書く。**画面とファイルを一致させる**のが自動保存の役目で、
            // 元の内容にちょうど戻ったときは「差分なし」の判定が書き込みを省く。
            mark_edited(app);
        }
        Message::Redo => {
            let Some(next) = app.redo.pop() else {
                return Task::none();
            };
            app.undo.push(snapshot(app));
            restore(app, next);
            mark_edited(app);
        }
        // `Ctrl+K`。**行末まで選択して、選択が空でないときだけ消す**（ADR-0016）。
        //
        // **`Binding::Sequence(vec![Select(End), Cut])` では成立しない。** `Select` は
        // アクションを publish するだけで `Content` はその場で変わらないのに、`Cut` は
        // **その場で `content.selection()` を読む**（`text_editor.rs:831`）。同じ
        // Sequence の中では Cut が見るのは選択前の状態で、いつも空。**選択だけが残って
        // 何も消えない**（実機で踏んだ）。だから選択と判定を同じ場所で順に行う。
        Message::CutToLineEnd => {
            use iced::advanced::text::editor::{Action, Edit, Motion};

            app.content.perform(Action::Select(Motion::End));
            // 行末・空行では選択が空になる。**ここで何もしないのが `Ctrl+K` の約束**
            // （`Backspace` で組むと、ここで手前の 1 文字が消える）。
            let Some(cut) = app.content.selection().filter(|s| !s.is_empty()) else {
                return Task::none();
            };
            // 削除は `Message::Edit` に通す。dirty の記録をここに二重に持たないため。
            let edited = update(app, Message::Edit(Action::Edit(Edit::Delete)));
            // Undo が無いので、切り取った分の戻し道はクリップボードだけ（ADR-0016）。
            return Task::batch([iced::clipboard::write(cut), edited]);
        }
        Message::Tick(now) => {
            if app.saved_flash_until.is_some_and(|until| now >= until) {
                app.saved_flash_until = None;
            }

            let mut task = Task::none();
            if let Some(dirty_since) = app.dirty_since
                && should_save(now, app.last_edit, dirty_since, app.retry_after)
            {
                // 失敗したら次の試行を後ろへ倒す。成功時は `clear_dirty` が畳む。
                // どちらも `finish_save` の仕事。**`Instant::now()` ではなく tick が
                // 持ってきた `now` を基準にする**ため、`InFlight` へ持ち回す
                // （境界をテストから組み立てられなくなる）。
                task = begin_save(app, None, Some(now));
            }

            // **保存を試みた「あと」に、独立して判定する。**
            //
            // 以前はここが `should_save` の `else if` だった。`DIRTY_MARKER_DELAY` は
            // `AUTOSAVE_MAX_WAIT` より長いので、marker の条件が真になるときは
            // 上限の条件も必ず真 → `else` 側へは原理的に到達せず、**異常時こそ点灯しない**
            // 安全装置になっていた。
            //
            // 保存が成功していれば `clear_dirty` が `dirty_since` を畳んでいるので、
            // ここに残っているのは「書こうとしたのに書けていない」状態だけ。
            if app
                .dirty_since
                .is_some_and(|since| now.duration_since(since) >= DIRTY_MARKER_DELAY)
            {
                app.show_marker = true;
            }
            return task;
        }
        Message::Saved(result) => return finish_save(app, result),
        Message::PaletteQueryChanged(query) => {
            // ⌘ 付きの打鍵はショートカット。`text_input` には `key_binding` が無いので
            // ここで受け取らない（ADR-0010）。入力欄の値は App が持っているので、
            // 捨てれば次の描画で元に戻る。**後から消すのではなく、そもそも反映しない。**
            if app.modifiers.command() {
                return Task::none();
            }
            if let Some(palette) = &mut app.palette {
                palette.matches = refilter(&app.notes, &query);
                palette.selected = 0;
                palette.query = query;
                // 候補が総入れ替えになってもスクロール位置は据え置かれる。戻さないと
                // 「選択は先頭なのに、その先頭が上に隠れている」逆の症状になる。
                return snap_palette_to_selected(0, palette.matches.len());
            }
        }
        Message::PaletteClose => app.palette = None,
        Message::TogglePreview => {
            // 出入りは表示の切り替えだけ。**編集ではない**ので dirty には触らない
            // （無編集の切替でディスクに書かない、と同じ規律）。
            app.preview = if app.preview.is_some() {
                None
            } else {
                Some(markdown::Content::parse(&app.content.text()))
            };
        }
        Message::LinkClicked(url) => {
            // macOS 専用アプリなので `open` を直接叩く（新しい依存を増やさない）。
            // 失敗は握りつぶさない。`.app` では stderr が誰にも見えない（ADR-0015 と同じ理由）。
            if let Err(e) = std::process::Command::new("/usr/bin/open").arg(&url).spawn() {
                app.error = Some(format!("リンクを開けません: {e}"));
            }
        }
        Message::NewNote => {
            // 作成もエディタを上書きする操作なので、切替と同じ保存ガードを通す。
            return begin_save(app, Some(PendingAction::NewNote), None);
        }
        Message::RenameStarted => {
            let Some(index) = app.selected else {
                app.error = Some("リネームするノートが開かれていません".to_string());
                return Task::none();
            };
            app.palette = None;
            let current = app.notes[index]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            app.rename = Some(current);
            return iced::widget::operation::focus(iced::widget::Id::new(RENAME_INPUT_ID));
        }
        Message::RenameChanged(name) => {
            // パレットと同じ理由で ⌘ 付きの打鍵は受け取らない（ADR-0010）。
            // ここは Enter でファイル名としてディスクに届くので、混入すると実害が残る。
            if app.modifiers.command() {
                return Task::none();
            }
            if let Some(current) = &mut app.rename {
                *current = name;
            }
        }
        Message::RenameCancel => app.rename = None,
        Message::RenameCommit => {
            let Some(input) = app.rename.clone() else {
                return Task::none();
            };
            // 名前を変える前に本文を書き戻す。ここを飛ばすと、未保存分が
            // 古いパス宛のまま宙に浮く。失敗したら中断（切替と同じガード）。
            return begin_save(app, Some(PendingAction::Rename(input)), None);
        }
        Message::DeleteNote => {
            app.palette = None;
            app.rename = None;
            // 捨てる前に書き戻す。`.trash` に残るのが「最後に自動保存された内容」ではなく
            // 「消す直前の内容」になり、誤って消しても打った分まで戻せる。
            // 失敗したら中断（切替・作成・リネームと同じガード）。
            return begin_save(app, Some(PendingAction::Delete), None);
        }
        // セットアップ中しか意味を持たない（入口の分岐で `update_setup` が処理する）。
        // 保存先が決まったあとに来ても、ここで vault を差し替えたりはしない。
        Message::SetupUseSuggested | Message::SetupBrowse | Message::SetupPicked(_) => {}
        Message::CloseRequested(id) => return close_window(app, id),
        // 入口で先に処理して return しているので、ここへは来ない
        // （`match` を網羅にしておくと、メッセージを足したとき取りこぼしが出ない）。
        Message::RetargetQuitMenu => {}
        Message::WindowUnfocused => app.modifiers = keyboard::Modifiers::empty(),
        Message::Key(event) => {
            // 修飾キーの状態を控える。**⌘ の keydown は文字キーより必ず先に届く**ので、
            // これで `text_input` 側の混入を順序に依存せず弾ける（ADR-0010）。
            if let keyboard::Event::ModifiersChanged(m) = event {
                app.modifiers = m;
                return Task::none();
            }
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return Task::none();
            };

            // `Cmd+P` で開く。**トグルにはしない。**
            //
            // subscription は**イベントを観測できても消費できない**ので、再押下で閉じても
            // 同じイベントが `text_input` にも渡って "p" が入力欄に挿入される
            // （「p が入ってから閉じる」という挙動になる）。iced に subscription 側から
            // イベントを `Captured` にする手段は無い。よって閉じる操作は
            // Escape / ✕ / 背景クリックに寄せた。
            if modifiers.command()
                && matches!(&key, keyboard::Key::Character(c) if c == "p")
                && app.palette.is_none()
            {
                // リネーム欄が開いていたら畳む（`RenameStarted` がパレットを畳むのと対称）。
                // 両方開いたままだと、下の `app.rename.is_some()` の分岐が Enter を先に拾い、
                // パレットで選んだつもりの Enter が `RenameCommit` として走る。
                app.rename = None;
                app.palette = Some(Palette {
                    query: String::new(),
                    matches: refilter(&app.notes, ""),
                    selected: 0,
                });
                // 開いた瞬間に入力欄へフォーカスを飛ばす。これが無いと「開いたのに打てない」。
                return iced::widget::operation::focus(iced::widget::Id::new(PALETTE_INPUT_ID));
            }

            // ⌘ のショートカットは 1 つの表にまとめる。`if` を並べると
            // 「どのキーがどの状況で効くか」がキーごとに散り、ガードの食い違い
            // （`Cmd+N` だけリネーム欄を畳んでいなかった類）が目で見つからなくなる。
            if modifiers.command() {
                use keyboard::key::Named;

                // **上に何か被さっている間は、見えていない画面を触らせない。**
                // ADR-0016 で Control 系をフォーカスで塞いだのと同じ理由。
                let bare = app.palette.is_none() && app.rename.is_none();
                let shortcut = match key.as_ref() {
                    // 新規作成・リネーム・退避はパレットが開いていても効く（中で畳む）。
                    keyboard::Key::Character("n") => Some(Message::NewNote),
                    keyboard::Key::Character("r") => Some(Message::RenameStarted),
                    keyboard::Key::Named(Named::Backspace) => Some(Message::DeleteNote),
                    // `Cmd+Shift+V` でエディタとプレビューを行き来する（ADR-0019）。
                    //
                    // **ここのトグルは成立する。** ADR-0004 で `Cmd+P` のトグルを諦めたのは、
                    // 消費できないイベントがフォーカス中の入力欄へ文字として混入するため。
                    // プレビュー中はエディタ自体が view に無く、再押下が漏れる先が無い。
                    // 開く方向も混入しない: `text_editor` への ⌘ 付き Insert は
                    // `editor_key_binding` が捨てる（ADR-0010）。
                    keyboard::Key::Character("v") if modifiers.shift() && bare => {
                        Some(Message::TogglePreview)
                    }
                    // `Cmd+Z` で元に戻す、`Cmd+Shift+Z` でやり直す（ADR-0017）。
                    // **プレビュー中も効かせない。** undo は見えていない本文を
                    // 書き換える操作になる（ADR-0019。表示は編集に戻ってから戻す）。
                    keyboard::Key::Character("z") if bare && app.preview.is_none() => Some(
                        if modifiers.shift() {
                            Message::Redo
                        } else {
                            Message::Undo
                        },
                    ),
                    _ => None,
                };
                if let Some(message) = shortcut {
                    return update(app, message);
                }
            }

            // リネーム入力中のキー。Enter で確定、Escape で取り消し。
            if app.rename.is_some() {
                match &key {
                    keyboard::Key::Named(keyboard::key::Named::Enter) => {
                        return update(app, Message::RenameCommit);
                    }
                    keyboard::Key::Named(keyboard::key::Named::Escape) => {
                        app.rename = None;
                        return Task::none();
                    }
                    _ => {}
                }
            }

            // プレビュー中の Escape は編集へ戻る。「一時的に被さったものは Escape で畳む」
            // の一貫（ADR-0004 / ADR-0019）。パレットが上に開いているときはそちらが先
            // （`handle_palette_key` が拾う）。リネームの Escape は上の分岐が先に返している。
            if app.preview.is_some()
                && app.palette.is_none()
                && matches!(&key, keyboard::Key::Named(keyboard::key::Named::Escape))
            {
                app.preview = None;
                return Task::none();
            }

            return handle_palette_key(app, &key, modifiers);
        }
    }
    Task::none()
}

/// subscription が拾った生イベントを `Message` へ写す。
///
/// **クロージャではなく名前付き関数なのは、`Cmd+W` の写像をテストから直接叩くため**
/// （ADR-0021）。`listen_with` は `fn` ポインタを取るので、そのまま渡せる。
///
/// `window` は `Cmd+W` を `Message::CloseRequested` に写すのに要る。この Id は
/// **subscription の第 3 引数でしか取れない**（`Message::Key` の側からは辿れない）ので、
/// ⌘ のショートカット表（`update()` 側）ではなくここで受ける。
fn map_event(
    event: iced::event::Event,
    _status: iced::event::Status,
    window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::event::Event::Keyboard(key_event) => {
            // **`Cmd+W` はメニューに食われず、ここまで普通のキーとして届いている。**
            // 既定メニューに Close 項目が無いせいで届け先が無く、不発だった（ADR-0021）。
            if is_close_shortcut(&key_event) {
                Some(Message::CloseRequested(window))
            } else {
                Some(Message::Key(key_event))
            }
        }
        // ⌘ を押したまま抜けると「離した」通知が来ない。押しっぱなしのまま戻ると
        // 入力欄が黙って無反応になるので、フォーカスを失った時点で解く（ADR-0010）。
        iced::event::Event::Window(iced::window::Event::Unfocused) => {
            Some(Message::WindowUnfocused)
        }
        _ => None,
    }
}

/// このキーイベントが「窓を閉じる」操作かどうか（`Cmd+W`）。
///
/// 判定を切り出してあるのは、**閉じる操作の取りこぼしと誤爆を実時間なしで固定する**ため。
/// `Message::Key` へ落ちたものは今までどおり本文やショートカット表へ流れるので、
/// ここで true を返した瞬間に 2 段階クローズ（ADR-0012）の入口が開く。
fn is_close_shortcut(event: &keyboard::Event) -> bool {
    matches!(
        event,
        keyboard::Event::KeyPressed {
            key,
            modifiers,
            repeat: false,
            ..
        } if key.as_ref() == keyboard::Key::Character("w")
            && *modifiers == keyboard::Modifiers::COMMAND
    )
}

/// キー入力と自動保存の tick。
///
/// キーは **`keyboard::listen()` ではなく `event::listen_with()` で拾う。**
/// 前者は `Status::Ignored` のイベントしか流さないので、パレットを開いた瞬間に
/// `text_input` へフォーカスが移り、そこで `Captured` になったキーが届かなくなる。
///
/// tick は **dirty（か、消すべき表示がある）ときだけ**回す。`Subscription` は state を見て
/// 出し分けられるので、何も編集していない間はタイマーがそもそも存在しない。
fn subscription(app: &App) -> Subscription<Message> {
    let keys = iced::event::listen_with(map_event);

    // 閉じる要求は **dirty かどうかに関わらず**常に拾う。dirty のときだけ購読すると、
    // 「閉じた瞬間に dirty が解ける」ような競合で取りこぼしたときに黙って終了する。
    let close = iced::window::close_requests().map(Message::CloseRequested);

    if is_dirty(app) || app.saved_flash_until.is_some() {
        Subscription::batch([keys, close, iced::time::every(TICK).map(Message::Tick)])
    } else {
        Subscription::batch([keys, close])
    }
}

fn folder_pane(app: &App) -> Element<'_, Message> {
    // フォルダ名は副文字（霞）。選ばれている行だけ生成りに持ち上げ、件数は常に霞のまま。
    let row_for = |label: &str, count: usize, selected: bool| {
        button(
            row![
                text(label.to_string())
                    .size(12)
                    .color(if selected { KINARI } else { KASUMI })
                    .width(Fill),
                text(count.to_string()).size(12).color(KASUMI),
            ]
            .padding(2),
        )
        .width(Fill)
        .style(if selected { selected_row } else { button::text })
    };

    let all_selected = app.selected_folder.is_none();
    let all = row_for("すべて", app.notes.len(), all_selected)
        .on_press(Message::FolderSelected(None));

    let items = app.folders.iter().map(|(name, count)| {
        let selected = app.selected_folder.as_deref() == Some(name.as_str());
        row_for(name, *count, selected)
            .on_press(Message::FolderSelected(Some(name.clone())))
            .into()
    });

    scrollable(column(std::iter::once(all.into()).chain(items)).spacing(1))
        .height(Fill)
        .into()
}

/// frontmatter のタグを 1 行の文字列にする。
///
/// `#` を前置するのは、プレビューやフォルダ名と同じ霞色のままでも
/// 「これはタグだ」と読み分けられるようにするため。琥珀は選択・一致文字の
/// ためのアクセントなので、副次情報のタグには回さない（ADR-0009）。
fn tag_label(tags: &[String]) -> String {
    tags.iter()
        .map(|t| format!("#{t}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn note_pane(app: &App) -> Element<'_, Message> {
    // 全件（約 1200 件）をそのまま並べる。仮想リストは要らないと spike で実測済み
    // （打鍵時の `view()` 構築 0.13ms）。無い機能を先回りで作らない。
    let rows = app.visible.iter().map(|&index| {
        let note = &app.notes[index];
        let selected = app.selected == Some(index);
        // タイトルは生成り、選択中だけ琥珀の明るい側へ。プレビューは常に霞。
        let mut body = column![
            text(&note.title)
                .size(13)
                .color(if selected { KOHAKU_SOFT } else { KINARI }),
            text(&note.preview).size(10).color(KASUMI),
        ]
        .spacing(2);

        // タグが無いノートには行そのものを足さない。約 1200 件のうち大半は
        // タグを持たないので、空行を常設すると一覧の密度だけが落ちる。
        if !note.tags.is_empty() {
            body = body.push(text(tag_label(&note.tags)).size(10).color(KASUMI));
        }

        button(body)
            .on_press(Message::NoteSelected(index))
            .width(Fill)
            .style(if selected { selected_row } else { button::text })
            .into()
    });

    scrollable(column(rows).spacing(1)).height(Fill).into()
}

/// `Cmd` 付きの打鍵を本文に入れない。
///
/// subscription はイベントを**観測できても消費できない**（ADR-0004）。`Cmd+P` は
/// パレットを開くと同時にこのエディタにも届き、iced の既定の `from_key_press` は
/// `text` があれば `Insert` を返す（`command()` のガードは `c/x/v/a` にしか無い）。
/// 放っておくと "p" が本文に入り、**自動保存でディスクまで届く**。
///
/// **`command()` で一律に落とさない。** `from_key_press` は `Insert` より先に
/// `Copy` / `Cut` / `Paste` / `SelectAll` を返すので、そこは通す。
///
/// あわせて **macOS の Control 系編集操作（`Ctrl+A` / `Ctrl+E` など）を補う**（ADR-0016）。
/// iced 0.14.2 の既定はこれを取りこぼす。**既定が答えを出せなかったときだけ**補うので、
/// 既定が正しく処理している組み合わせ（`Ctrl+H` = Backspace、Linux の `Ctrl+A` = 全選択）は触らない。
///
/// `Ctrl+Shift+<移動系>` は選択の拡張（ADR-0023）。ADR-0016 は `Ctrl+Shift+A` を意図的に
/// 弾いていたが、それは「未定義のまま拾う」ことへの用心であって「選択には使わない」という
/// 決定ではなかった。iced 既定の矢印キーが shift で `Move` と `Select` を切り替えるのと
/// 揃えて、`macos_control_binding` 側で同じ切り替えを行う。
fn editor_key_binding(kp: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    let command = kp.modifiers.command();
    // **フォーカスが無いときは補わない。** `key_binding` はフォーカスの有無に関わらず
    // 呼ばれる（`Status::Active` で来る）。パレット・リネーム欄を開いている間に
    // `Ctrl+N` で裏のカーソルが動いたり、`Ctrl+D` で**見ていない本文が消える**のを防ぐ。
    // これでパレットの `ctrl-n` / `ctrl-p`（一覧移動）とも衝突しない。
    let focused = matches!(kp.status, text_editor::Status::Focused { .. });
    // **`Modifiers::CTRL` または `Ctrl+Shift` との完全一致で見る。** `control()` だと
    // `Cmd+Ctrl+A` まで拾ってしまう（iced の `convert_macos_shortcut` も完全一致）。
    let control_only = kp.modifiers == keyboard::Modifiers::CTRL;
    let control_shift = kp.modifiers == keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT;
    let key = kp.key.clone();

    match text_editor::Binding::from_key_press(kp) {
        Some(text_editor::Binding::Insert(_)) if command => None,
        Some(other) => Some(other),
        None if focused && control_only => macos_control_binding(&key, false),
        None if focused && control_shift => macos_control_binding(&key, true),
        None => None,
    }
}

/// macOS の Control 系編集操作。**アプリ側で定義しないと使えない**（ADR-0016）。
///
/// `text_editor` は Cocoa のネイティブテキスト部品ではないので、macOS の
/// システム設定やユーザー辞書（`DefaultKeyBinding.dict`）では直らない。
/// iced 0.14.2 の既定は `Ctrl+A` を `Home` へ変換しておきながら、その後の分岐で
/// **変換前のキーと `text`（macOS では制御文字が入る）を見てしまい `None` を返す**。
///
/// `Ctrl+K`（行末まで切り取り）だけは `Binding` を組み合わせず**メッセージにして
/// `update()` で処理する**（ADR-0016）。`Sequence` の中では `Select` の結果を
/// `Cut` が見られないため（`Message::CutToLineEnd` の分岐に理由を書いた）。
/// `select` は `Ctrl+Shift+<key>` かどうか。移動系（a/e/b/f/n/p）だけが対応し、
/// `d`（削除）・`k`（切り取り）は shift 版の意味を定義していないので `None` で見送る
/// （ADR-0023）。
fn macos_control_binding(
    key: &keyboard::Key,
    select: bool,
) -> Option<text_editor::Binding<Message>> {
    use iced::advanced::text::editor::Motion;

    let motion = match key.as_ref() {
        keyboard::Key::Character("a") => Motion::Home,
        keyboard::Key::Character("e") => Motion::End,
        keyboard::Key::Character("b") => Motion::Left,
        keyboard::Key::Character("f") => Motion::Right,
        keyboard::Key::Character("n") => Motion::Down,
        keyboard::Key::Character("p") => Motion::Up,
        // 後ろを 1 文字消す。`Ctrl+H`（前を 1 文字）は既定が処理できているので触らない。
        keyboard::Key::Character("d") if !select => return Some(text_editor::Binding::Delete),
        // 行末まで切り取る。行を繋げたいときは続けて `Ctrl+D` を押す（doc 参照）。
        keyboard::Key::Character("k") if !select => {
            return Some(text_editor::Binding::Custom(Message::CutToLineEnd));
        }
        _ => return None,
    };
    Some(if select {
        text_editor::Binding::Select(motion)
    } else {
        text_editor::Binding::Move(motion)
    })
}

fn editor_pane(app: &App) -> Element<'_, Message> {
    // **未選択でエディタを出さない**（ADR-0022）。出すと、打てるのに `mark_edited` が
    // dirty を立てず、ノートを開いた瞬間に無警告で消える（ADR-0002 で廃止した経路と同型）。
    // 選択が無いのは vault が 0 件のときだけ。
    if app.selected.is_none() {
        return empty_pane();
    }
    if let Some(preview) = &app.preview {
        return preview_pane(preview);
    }
    text_editor(&app.content)
        .id(iced::widget::Id::new(EDITOR_ID))
        .on_action(Message::Edit)
        .key_binding(editor_key_binding)
        .font(EDITOR_FONT)
        // 日本語には単語境界がほぼ無く、既定の `Word` だと長い段落が 1 つの巨大な単語になる。
        .wrapping(iced::advanced::text::Wrapping::WordOrGlyph)
        // syntect の既製テーマではなく自前の Markdown ハイライタ（`highlight.rs` の doc 参照）。
        .highlight_with::<highlight::MarkdownHighlighter>((), markdown_format)
        .style(editor_style)
        .height(Fill)
        .into()
}

/// ノートが 1 件も無いときの面。エディタと同じ淡墨に、`Cmd+N` の案内だけを置く。
///
/// **ここに編集できるものを置かない。** 置いた瞬間に「打てるのに保存されない」経路が戻る。
fn empty_pane() -> Element<'static, Message> {
    container(text(EMPTY_VAULT).size(13).color(KASUMI))
        .center_x(Fill)
        .center_y(Fill)
        .style(|_theme: &iced::Theme| container::background(TANBOKU))
        .into()
}

/// 読み取り専用プレビュー（`Cmd+Shift+V`。ADR-0019）。エディタと同じ面（淡墨）に描く。
///
/// パース済みの [`markdown::Content`] を並べるだけで、ここでは何も計算しない
/// （パースは `TogglePreview` と `replace_content` がやる）。
fn preview_pane(preview: &markdown::Content) -> Element<'_, Message> {
    // 戻り方は常時表示する（パレットの「esc・✕・背景クリックで閉じる」と同じ流儀。ADR-0004）。
    let hint = text("プレビュー ─ esc / ⌘⇧V で編集に戻る").size(10).color(KASUMI);

    let body = markdown::view(preview.items(), preview_settings()).map(Message::LinkClicked);

    container(column![hint, scrollable(body).height(Fill).width(Fill)].spacing(8))
        .style(|_theme: &iced::Theme| container::Style {
            background: Some(TANBOKU.into()),
            ..container::Style::default()
        })
        .padding(12)
        .height(Fill)
        .width(Fill)
        .into()
}

/// プレビューの意匠。幽玄の面に合わせ、既定から動かすのは次の 3 点:
///
/// - **書体は本文もコードも `EDITOR_FONT` を名指しする。** コードの既定
///   `Font::MONOSPACE` は cosmic-text が漢字に GB18030 Bitmap を選んで**漢字だけ消える**
///   （ADR-0003 / ADR-0013 の罠）。本文も `Font::DEFAULT` のままだと**イタリック span の
///   漢字が豆腐になる**（`*斜体*` の 2 文字が横縞グリフに化けるのを実測）。CJK に
///   italic face は無く、名指しのファミリなら立体のまま描かれて文字は消えない
/// - 色は幽玄の値に置き換える（既定は白文字 + `#111111` の地で、淡墨の面では浮く。
///   リンクは琥珀 = 「押せる」合図の色に寄せる。ADR-0009）
fn preview_settings() -> markdown::Settings {
    markdown::Settings::with_style(markdown::Style {
        font: EDITOR_FONT,
        inline_code_highlight: markdown::Highlight {
            background: KOBOKU.into(),
            border: iced::border::rounded(4),
        },
        inline_code_padding: iced::padding::left(2).right(2),
        inline_code_color: KOHAKU_SOFT,
        inline_code_font: EDITOR_FONT,
        code_block_font: EDITOR_FONT,
        link_color: KOHAKU,
    })
}

/// マッチした文字だけ色を変えた span を作る。タイトル行と本文の抜粋行で共有する。
///
/// 範囲は**バイト範囲**なので `get()` で受ける。日本語で文字境界を跨いだときに
/// panic しないため（`&text[range]` だと落ちる）。
///
/// 地の色を引数で受けるのは、タイトル（生成り）と抜粋（霞）で違うから。
fn highlight_spans(
    text: &str,
    ranges: &[std::ops::Range<usize>],
    base: Color,
) -> Vec<iced::advanced::text::Span<'static, ()>> {
    // 一致文字は琥珀の明るい側 + セミボールド。色だけだと霞んだ地の上で見落とすことがある。
    let hit_font = Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    };
    let mut spans: Vec<iced::advanced::text::Span<'static, ()>> = Vec::new();
    let mut last = 0;

    for range in ranges {
        if range.start > last
            && let Some(plain) = text.get(last..range.start)
        {
            spans.push(span(plain.to_string()).color(base));
        }
        if let Some(matched) = text.get(range.clone()) {
            spans.push(span(matched.to_string()).color(KOHAKU_SOFT).font(hit_font));
        }
        last = range.end;
    }
    if let Some(rest) = text.get(last..) {
        spans.push(span(rest.to_string()).color(base));
    }
    spans
}

/// 当たった文字だけ色を変えた 1 行。候補のタイトル行と本文の抜粋行で共有する。
///
/// **折り返させない。** 行の高さが候補ごとに変わると、相対オフセットでの追従
/// （`snap_palette_to_selected`）が前提を失う。
fn hit_text(
    body: &str,
    ranges: &[std::ops::Range<usize>],
    base: Color,
    size: f32,
) -> Element<'static, Message> {
    rich_text(highlight_spans(body, ranges, base))
        .size(size)
        .wrapping(iced::advanced::text::Wrapping::None)
        .into()
}

/// スクリム + 中央パネル。iced にモーダル用の標準ウィジェットは無いので自前に組む。
///
/// スクリムは**背景側だけを覆う層**として敷き、その上にパネルを重ねる。パネルごと
/// `mouse_area` で包むと、パネル内のクリックまで「背景クリック」として拾ってしまう。
fn overlay(
    panel: Element<'static, Message>,
    dismiss: Message,
    pad: u16,
) -> Element<'static, Message> {
    let scrim = mouse_area(
        container(iced::widget::Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_theme: &iced::Theme| {
                // 濃墨よりさらに深い墨でぼかす。真っ黒ではなく地の色相を保つ。
                container::background(Color::from_rgba8(0x0A, 0x09, 0x0B, 0.6))
            }),
    )
    .on_press(dismiss);

    stack![scrim, container(panel).center_x(Fill).padding(pad)].into()
}

/// エラーの 1 行。セットアップ画面と本画面で同じ見た目にする。
///
/// **エラーも枯茶。赤を持ち込まない**（ADR-0009）。出すものが無くても
/// 空の `text` を置いて、行の有無でレイアウトが跳ねないようにする。
fn error_line(error: Option<&String>) -> Element<'static, Message> {
    match error {
        Some(message) => text(format!("⚠ {message}")).size(11).color(KARACHA).into(),
        None => text("").size(11).into(),
    }
}

/// 浮きパレット本体。iced にモーダル用の標準ウィジェットは無いので、
/// 「画面いっぱいの半透明コンテナ（スクリム）＋中央のパネル」を `stack` で自前に組む。
fn palette_overlay(app: &App, palette: &Palette) -> Element<'static, Message> {
    let input = text_input("ノートを検索…", &palette.query)
        .id(iced::widget::Id::new(PALETTE_INPUT_ID))
        .on_input(Message::PaletteQueryChanged)
        .padding(10)
        .size(15);

    let rows = palette
        .matches
        .iter()
        .enumerate()
        .map(|(row, hit)| {
            let note = &app.notes[hit.note];
            let is_selected = row == palette.selected;

            // **2 行目はどちらか一方。** 本文に当たった行では、フォルダ・タグより
            // 「なぜ当たったか」を優先する（フォルダは開けば分かる）。両方出すと
            // 幅 640px を食い、折り返して行の高さが変わる。
            let (title_ranges, second): (&[std::ops::Range<usize>], Element<'static, Message>) =
                match &hit.kind {
                    HitKind::Title(m) => {
                        // タグはフォルダ名と同じ行に併記する。行を足すとタグの有無で
                        // 行の高さが変わり、相対オフセットの追従が前提を失う。
                        let meta = if note.tags.is_empty() {
                            note.folder.clone()
                        } else {
                            format!("{} · {}", note.folder, tag_label(&note.tags))
                        };
                        (
                            &m.ranges,
                            text(meta)
                                .size(10)
                                .color(KASUMI)
                                .wrapping(iced::advanced::text::Wrapping::None)
                                .into(),
                        )
                    }
                    HitKind::Body { snippet, hit } => {
                        (&[], hit_text(snippet, std::slice::from_ref(hit), KASUMI, 10.0))
                    }
                };

            container(
                iced::widget::column![hit_text(&note.title, title_ranges, KINARI, 13.0), second]
                    .spacing(1),
            )
            .padding(6)
            .width(Fill)
            .style(move |_theme: &iced::Theme| {
                if is_selected {
                    container::background(KOHAKU_WASH)
                } else {
                    container::Style::default()
                }
            })
            .into()
        })
        .collect::<Vec<_>>();

    // 閉じ方が見えていること自体が実用上効く（`Cmd+P` のトグルが使えないため）。
    let header = row![
        input,
        button(text("✕").size(15))
            .on_press(Message::PaletteClose)
            .padding(8)
            .style(button::text),
    ]
    .spacing(4);

    let panel = container(
        column![
            header,
            scrollable(column(rows).spacing(1))
                .id(iced::widget::Id::new(PALETTE_LIST_ID))
                .height(Length::Fixed(360.0)),
            text(format!(
                "{} 件中 上位 {} 件 / ctrl-n・ctrl-p で移動、enter で開く、esc・✕・背景クリックで閉じる",
                app.notes.len(),
                palette.matches.len()
            ))
            .size(10)
            .color(KASUMI),
        ]
        .spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(640.0))
    .style(panel_style);

    overlay(panel.into(), Message::PaletteClose, 80)
}

/// リネームの入力欄。パレットと同じ「スクリム + 中央パネル」の作りに揃える。
fn rename_overlay(current: &str) -> Element<'static, Message> {
    let input = text_input("新しいファイル名", current)
        .id(iced::widget::Id::new(RENAME_INPUT_ID))
        .on_input(Message::RenameChanged)
        .on_submit(Message::RenameCommit)
        .padding(10)
        .size(15);

    let panel = container(
        column![
            text("ファイル名を変更").size(13),
            input,
            // 「リネームしたのに一覧の見た目が変わらない」を先に説明しておく。
            // タイトルは frontmatter / 見出しが優先されるため、仕様どおりでも驚く。
            text("変えるのはファイル名だけ。一覧のタイトルは frontmatter の title: や本文の # 見出しが優先されます")
                .size(10)
                .color(KASUMI),
            text("enter で確定、esc・✕・背景クリックで取り消し").size(10).color(KASUMI),
        ]
        .spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(520.0))
    .style(panel_style);

    overlay(panel.into(), Message::RenameCancel, 120)
}

/// 初回セットアップの画面（ADR-0015）。Boostnote に倣い、**候補を見せてから選ばせる**。
///
/// OS のフォルダ選択をいきなり開かないのは、「何を選ばされているのか」が分からないまま
/// Finder の窓が出る形を避けるため。
fn setup_view(setup: &Setup) -> Element<'_, Message> {
    let error = error_line(setup.error.as_ref());

    let panel = container(
        column![
            text("メモの保存先を決める").size(20).font(HEADING_FONT),
            text("haboku は、選んだフォルダの中の Markdown ファイルをそのまま読み書きします。既にメモが入っているフォルダを選べば、そのまま開きます。")
                .size(12)
                .color(KASUMI),
            container(text(setup.suggested.display().to_string()).size(13).color(KINARI))
                .padding(10)
                .width(Fill)
                .style(|_theme: &iced::Theme| container::background(TANBOKU)),
            row![
                button(text("この場所で始める").size(13))
                    .on_press(Message::SetupUseSuggested)
                    .padding([8, 14])
                    .style(button::primary),
                button(text("別の場所を選ぶ…").size(13))
                    .on_press(Message::SetupBrowse)
                    .padding([8, 14])
                    .style(button::text),
            ]
            .spacing(8),
            // **あとで変えられることを先に言う。** 決めきれずに止まるのを防ぐ。
            text("この選択は覚えます。変えたいときは選び直せます")
                .size(10)
                .color(KASUMI),
            error,
        ]
        .spacing(12),
    )
    .padding(20)
    .width(Length::Fixed(560.0))
    .style(panel_style);

    container(panel).center_x(Fill).center_y(Fill).padding(40).into()
}

fn view(app: &App) -> Element<'_, Message> {
    let t0 = Instant::now();

    // 保存先が決まるまでは 3 ペインを組まない（そもそも見せるノートが無い）。
    if let Some(setup) = &app.setup {
        let element = setup_view(setup);
        app.last_view_us.set(t0.elapsed().as_micros());
        return element;
    }

    let panes = row![
        container(folder_pane(app)).width(Length::Fixed(SIDEBAR_WIDTH)),
        container(note_pane(app)).width(Length::Fixed(LIST_WIDTH)),
        container(editor_pane(app)).width(Fill),
    ]
    .spacing(8)
    .height(Fill);

    // 保存状態は色でも語る。未保存・編集中は枯茶（警告の色）、保存済みは琥珀の明るい側。
    let (state_label, state_color) = if app.show_marker {
        ("● 未保存", KARACHA)
    } else if is_dirty(app) {
        ("… 編集中", KARACHA)
    } else if app.saved_flash_until.is_some() {
        ("保存しました", KOHAKU_SOFT)
    } else {
        ("―", KASUMI)
    };

    let status = row![
        text(format!("{} notes", app.visible.len())).size(11).color(KASUMI),
        text(format!("load {:.1}ms", app.load_ms)).size(11).color(KASUMI),
        // Done の定義は 1ms 未満。ここが太りだしたら `view()` に重い処理が入った合図。
        text(format!(
            "view {:.2}ms",
            app.last_view_us.get() as f64 / 1000.0
        ))
        .size(11)
        .color(KASUMI),
        // 編集していないのに増えるなら「無編集でも書いている」ということ。
        text(format!("saves {}", app.saves)).size(11).color(KASUMI),
        text(state_label).size(11).color(state_color),
    ]
    .spacing(16);

    let error = error_line(app.error.as_ref());

    // **高さを固定する。** 可変にすると表示の桁数が変わるたびに下段の高さが動き、
    // 「打った文字が1つ上の行に入った」ように見える（`Fill` の隣に可変長を置く罠）。
    let base: Element<'_, Message> = column![
        panes,
        container(status).height(Length::Fixed(18.0)),
        container(error).height(Length::Fixed(16.0)),
    ]
    .spacing(6)
    .padding(8)
    .into();

    // パレットとリネームは同時に開かない（開くときに互いを畳んでいる）。
    let element = match (&app.palette, &app.rename) {
        (Some(palette), _) => stack![base, palette_overlay(app, palette)].into(),
        (None, Some(name)) => stack![base, rename_overlay(name)].into(),
        (None, None) => base,
    };

    app.last_view_us.set(t0.elapsed().as_micros());
    element
}

fn main() -> ExitCode {
    let home = home_dir();
    let config_file = config::config_file(&home);

    let choice = resolve_vault(
        std::env::var("VAULT").ok(),
        config::read_vault(&config_file),
        config::default_vault(&home),
    );

    // 保存先が決まっているときだけ、iced を起動する前に読み込む。
    // **時間を測るのに UI の初期化を混ぜたくない**（初回セットアップ経由の分は
    // `adopt_vault` が測る。Done の定義の 50ms は 2 回目以降の起動で見る）。
    // boot は `(App, Task)` を返せる（`IntoBoot`）。**起動直後に 1 回だけ**
    // Quit 項目の付け替えを流すのにこれを使う。AppKit はメインスレッドからしか
    // 触れないので、`main` の中ではなく `update()` の中で実行させる（ADR-0021）。
    let boot_app: Box<dyn Fn() -> (App, Task<Message>)> = match choice {
        VaultChoice::Refuse(msg) => {
            eprintln!("haboku: {msg}");
            return ExitCode::from(2);
        }
        VaultChoice::Ready(root) => {
            let t0 = Instant::now();
            let load = vault::load_dir(&root);
            let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "vault: {} notes / {} 件読めず / {load_ms:.1}ms / {}",
                load.notes.len(),
                load.failed,
                root.display()
            );
            // **`load` を clone しない。** vault 全文（約 1200 件ぶんの `String`）の
            // deep copy になる。しかもこのクロージャは `Fn` として iced に握られたまま
            // プロセス終了まで生きるので、clone だと App の実体とは別に
            // vault 1 本ぶんが常駐し続ける。呼ばれるのは 1 回なので取り出して渡す。
            let load = std::cell::RefCell::new(Some(load));
            Box::new(move || {
                let load = load.borrow_mut().take().expect("boot は 1 回しか呼ばれない");
                (
                    boot(root.clone(), load, load_ms),
                    Task::done(Message::RetargetQuitMenu),
                )
            })
        }
        VaultChoice::NeedsSetup { suggested } => {
            eprintln!("vault: 未設定（初回セットアップを表示します）");
            Box::new(move || {
                (
                    boot_setup(suggested.clone()),
                    Task::done(Message::RetargetQuitMenu),
                )
            })
        }
    };

    // テーマは一度だけ組む。`theme()` は描画のたびに呼ばれるので、そこで
    // `Theme::custom`（Arc 生成 + extended palette の導出）を回さない。
    let theme = yugen_theme();

    let result = iced::application(boot_app, update, view)
    .title("haboku")
    .theme(move |_app: &App| theme.clone())
    .subscription(subscription)
    // **既定（true）だと、閉じる要求はアプリに届く前にプロセスを終わらせる。**
    // デバウンス（1 秒）の途中で閉じた分の編集が、ディスクにもメモリにも残らず消える。
    // false にして `Message::CloseRequested` を自分で処理し、保存を挟む。
    .exit_on_close_request(false)
    .run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("haboku: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haboku::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    /// 使い捨て vault を作って `App` を組み立てる。
    ///
    /// **実データを自動保存の実験台にしない。** 本物のノートが壊れる。
    fn app_with_vault(name: &str) -> (TempDir, App) {
        let dir = TempDir::new(name);

        // mtime 降順で並ぶので、後に書いたほうが index 0 に来る。
        for (i, title) in ["古い", "新しい"].iter().enumerate() {
            let body = format!("---\ntitle: \"{title}\"\n---\n\n# {title}\n\n日本語の本文。\n");
            std::fs::write(dir.join(format!("note-{i}.md")), body).unwrap();
        }

        let load = vault::load_dir(&dir);
        assert_eq!(load.notes.len(), 2);
        let app = boot(dir.to_path_buf(), load, 0.0);
        (dir, app)
    }

    /// テストからメッセージを流す。`update` が返す `Task` は iced ランタイムへ返すためのもので、
    /// ここでは実行するものが無いので捨てる。
    ///
    /// **保存だけは捨てると走らない**（ADR-0014 で `Task` へ出したため）。書き込みを伴う
    /// テストは `send` のあとに `flush_saves` を呼ぶこと。
    fn send(app: &mut App, message: Message) {
        let _ = update(app, message);
    }

    /// iced のランタイムの代わりに、積まれた保存を**同期で**実行して `Saved` を流す。
    ///
    /// 差し替えているのは「`vault::save` をどのスレッドで走らせるか」だけで、
    /// 何を書くか（`InFlight`）も完了後の分岐（`finish_save`）も本番と同じ経路を通る。
    /// テストが確かめたいのは状態機械であって tokio ではない。
    ///
    /// **成功した保存は次の保存を積み得る**（保存中の打鍵分・待たせていた操作）ので、
    /// 積まれなくなるまで回す。
    fn flush_saves(app: &mut App) {
        // 保存が保存を呼び続けることはない（`begin_save` は差分が無ければ書かない）が、
        // 万一そうなったときにテストが無限に回るより落ちたほうがよい。
        for _ in 0..8 {
            let Some(in_flight) = app.saving.as_ref() else {
                return;
            };
            let result =
                vault::save(&in_flight.path, &in_flight.contents).map_err(|e| e.to_string());
            send(app, Message::Saved(result));
        }
        panic!("保存が終わらない（`begin_save` が書くべき差分を畳めていない）");
    }

    /// フォルダ違いのノートを持つ vault を作る。`(フォルダ, タイトル)` の順で置き、
    /// 後に書いたものほど新しい = 一覧の上に来る。
    fn app_with_folders(name: &str, entries: &[(&str, &str)]) -> (TempDir, App) {
        let dir = TempDir::new(name);

        for (i, (folder, title)) in entries.iter().enumerate() {
            let sub = dir.join(folder);
            std::fs::create_dir_all(&sub).unwrap();
            let body = format!("---\ntitle: \"{title}\"\n---\n\n本文。\n");
            std::fs::write(sub.join(format!("n-{i}.md")), body).unwrap();
        }

        let app = boot(dir.to_path_buf(), vault::load_dir(&dir), 0.0);
        (dir, app)
    }

    /// **読めなかったノートが画面に出ること。**
    ///
    /// `.app` の利用者に stderr は見えないので、常駐エラーが唯一の通知経路になる。
    /// 出さないと「権限で欠けた一覧」と「もともとその件数しかない vault」を区別できない
    /// （`docs/spec.md` の「読めないファイルは黙って捨てず件数に出す」）。
    #[test]
    fn unreadable_notes_surface_on_screen() {
        let dir = TempDir::new("unreadable");
        std::fs::write(dir.join("読める.md"), "# 読める").unwrap();
        let locked = dir.join("鍵付き");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("読めない.md"), "# 読めない").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let app = boot(dir.to_path_buf(), vault::load_dir(&dir), 0.0);

        assert_eq!(app.notes.len(), 1, "読めるノートまで落としている");
        let error = app.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("1 件") && error.contains("鍵付き"),
            "読めなかったことが画面に出ていない: {error:?}"
        );

    }

    fn insert(c: char) -> Message {
        Message::Edit(text_editor::Action::Edit(text_editor::Edit::Insert(c)))
    }

    fn key(c: &str) -> keyboard::Key {
        keyboard::Key::Character(c.into())
    }

    fn named(k: keyboard::key::Named) -> keyboard::Key {
        keyboard::Key::Named(k)
    }

    /// パレットを開いた状態にする（`Cmd+P` の処理と同じ初期値）。
    fn open_palette(app: &mut App, query: &str) {
        app.palette = Some(Palette {
            query: query.to_string(),
            matches: refilter(&app.notes, query),
            selected: 0,
        });
    }

    fn mtime(path: &std::path::Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    /// 編集して手を止めたら、デバウンス経過でディスクに書かれること。
    #[test]
    fn autosave_writes_after_debounce() {
        let (_dir, mut app) = app_with_vault("writes");
        send(&mut app,Message::NoteSelected(0));
        send(&mut app,insert('X'));
        assert!(is_dirty(&app), "編集したのに dirty が立っていない");

        send(&mut app,Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);

        assert_eq!(app.saves, 1, "デバウンス経過後に保存されていない");
        assert!(!is_dirty(&app), "保存後も dirty が残っている");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(on_disk.contains('X'), "編集がディスクに届いていない");
    }

    /// **書き込みが `update()` の中で終わっていないこと。** これが ADR-0014 の要求そのもの。
    ///
    /// `vault::save` は `sync_all` でディスクを待つ。`update()` は UI スレッドで走るので、
    /// ここで書き終えているなら **`sync_all` の間ずっと画面が止まっている**。遅い vault
    /// （iCloud Drive・外付け・ネットワーク共有）では数秒単位になる。
    ///
    /// 「固まらないこと」は単体テストから直接は測れないので、**書き込みが `update()` の
    /// 外にいること**を固定する。同期保存へ戻したらここが落ちる。
    #[test]
    fn the_disk_write_does_not_happen_inside_update() {
        let (_dir, mut app) = app_with_vault("write-outside-update");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));

        assert!(app.saving.is_some(), "保存が投げられていない");
        assert_eq!(app.saves, 0, "update の中で保存を完了している");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(
            !on_disk.contains('X'),
            "update の中でディスクへ書いている（UI スレッドで sync_all を待っている）"
        );

        flush_saves(&mut app);
        assert_eq!(app.saves, 1, "投げた保存が完了していない");
    }

    /// **保存が飛んでいる最中の打鍵を、保存済み扱いにしないこと。**
    ///
    /// 非同期にした代償がここに出る。書き出すのは投げた瞬間のスナップショットなので、
    /// 完了時にエディタの中身がそれより新しければ **dirty を畳んではいけない**。
    /// 畳むと、次の切替で「保存済み」と判断されて打った分が消える。
    #[test]
    fn edits_made_while_saving_are_kept_dirty() {
        let (_dir, mut app) = app_with_vault("edit-while-saving");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        assert!(app.saving.is_some(), "保存が投げられていない");

        // **保存中でも打てること自体が要求。** 同期保存ならここには到達しない。
        send(&mut app, insert('Y'));
        flush_saves(&mut app);

        assert_eq!(app.saves, 1, "投げた保存が完了していない");
        assert!(is_dirty(&app), "保存中に打った分まで保存済み扱いになっている");
        assert!(app.content.text().contains('Y'), "保存中の打鍵が消えた");

        // 次のデバウンスで、遅れた分もちゃんと届く。
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);
        assert!(!is_dirty(&app), "追いつきの保存が走っていない");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(on_disk.contains('Y'), "保存中に打った分がディスクに届いていない");
    }

    /// **保存中に来た切替は、保存が通ってから実行されること。**
    ///
    /// ガードの意味論（保存できるまでエディタを潰さない）は非同期でも変わらない。
    /// 変わったのは判定の場所だけで、待っている間も UI は生きている。
    #[test]
    fn a_switch_requested_while_saving_waits_for_it() {
        let (_dir, mut app) = app_with_vault("switch-while-saving");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        let saving_path = app.saving.as_ref().expect("保存が投げられていない").path.clone();

        send(&mut app, Message::NoteSelected(1));

        assert_eq!(app.selected, Some(0), "保存の完了を待たずに切り替わった");
        assert!(app.pending.is_some(), "切替が保留されていない");
        assert_eq!(app.saves, 0, "保存が二重に走っている");

        flush_saves(&mut app);

        assert_eq!(app.selected, Some(1), "保存が通ったのに切り替わらない");
        let on_disk = std::fs::read_to_string(&saving_path).unwrap();
        assert!(on_disk.contains('X'), "切替前の編集がディスクに届いていない");
    }

    /// **何も編集せずノートを切り替えたら、ディスクに書かないこと。**
    ///
    /// mtime が動くと外部ツール（git・同期）から「更新された」と誤認される。
    /// Done の定義に明記された禁止事項。
    #[test]
    fn switching_without_editing_does_not_touch_the_file() {
        let (_dir, mut app) = app_with_vault("no-write");
        send(&mut app,Message::NoteSelected(0));
        let before = mtime(&app.notes[0].path);

        send(&mut app,Message::NoteSelected(1));

        assert_eq!(app.saves, 0, "無編集なのに書き込んでいる");
        assert_eq!(mtime(&app.notes[0].path), before, "無編集なのに mtime が動いた");
    }

    /// **保存に失敗したらノート切替を中断し、編集内容をエディタに残すこと。**
    ///
    /// ここで進むと、失われた本文はディスクにもメモリにも残らない。前身でデータ喪失を
    /// 招いた欠陥がこれ。失敗系は失敗させないと検証できないので、実際に書けなくする。
    #[test]
    fn failed_save_blocks_the_switch_and_keeps_the_text() {

        let (dir, mut app) = app_with_vault("save-fails");
        send(&mut app,Message::NoteSelected(0));
        send(&mut app,insert('X'));

        // ディレクトリを書き込み不可にする。`vault::save` は一時ファイルを作ってから
        // rename するので、作成の時点で失敗する。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        send(&mut app,Message::NoteSelected(1));
        flush_saves(&mut app);

        assert_eq!(app.selected, Some(0), "保存に失敗したのに切り替わった");
        assert!(app.error.is_some(), "保存失敗が表に出ていない");
        assert!(is_dirty(&app), "保存できていないのに dirty が畳まれている");
        assert!(
            app.content.text().contains('X'),
            "未保存の編集内容がエディタから消えた"
        );

    }

    /// **ウィンドウを閉じるときに、デバウンス途中の編集が書き戻されること。**
    ///
    /// 切替・作成・リネーム・削除には保存ガードがあったのに、「閉じる」だけが
    /// 素通りしていた。`Cmd+W` を打った瞬間に直前 1 秒分が消えるので、
    /// 一番踏みやすいデータ喪失の穴だった。
    #[test]
    fn closing_the_window_saves_pending_edits() {
        let (_dir, mut app) = app_with_vault("close-saves");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        assert!(is_dirty(&app), "編集したのに dirty が立っていない");

        // デバウンス（1 秒）を待たずに閉じる。ここが実際の操作と同じ条件。
        let _ = close_window(&mut app, iced::window::Id::unique());
        flush_saves(&mut app);

        assert_eq!(app.saves, 1, "閉じるときに保存されていない");
        assert!(!is_dirty(&app), "保存したのに dirty が残っている");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(on_disk.contains('X'), "閉じる直前の編集がディスクに届いていない");
    }

    /// **保存に失敗したら、1 回目は閉じないこと。** 黙って終了すると、切替を中断してまで
    /// 守った本文が最後の一歩で消える。
    ///
    /// Task は中身を覗けないので、「閉じなかった」ことは踏みとどまった痕跡
    /// （dirty が残る・エラーが出る・未保存マーカーが立つ）で確かめる。
    #[test]
    fn first_close_attempt_is_refused_when_saving_fails() {

        let (dir, mut app) = app_with_vault("close-blocked");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _ = close_window(&mut app, iced::window::Id::unique());
        flush_saves(&mut app);

        assert!(is_dirty(&app), "保存できていないのに dirty が畳まれている");
        assert!(app.show_marker, "閉じられない理由が画面に出ていない");
        assert!(app.content.text().contains('X'), "未保存の編集がエディタから消えた");
        // **次に何が起きるかまで伝わっていること。** 「閉じられない」だけでは打つ手がない。
        let error = app.error.as_deref().unwrap_or_default();
        assert!(error.contains("退避"), "2 回目に何が起きるかが伝わっていない: {error}");

    }

    /// **2 回目の要求では、本文を退避してから閉じること。**
    ///
    /// 踏みとどまり続けると強制終了しか出口が無くなり、結局全部失う。保存が失敗する原因
    /// （容量・ドライブ・権限）は待っても直らないので、2 回目は必ず閉じる。ただし黙っては捨てない。
    ///
    /// ノートのあるフォルダだけを書けなくして、**退避先が vault ルートへ落ちる**ことも同時に見る。
    #[test]
    fn second_close_attempt_rescues_the_text_before_closing() {

        let (dir, mut app) = app_with_folders("close-rescue", &[("topics", "消えては困る")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        let id = iced::window::Id::unique();
        let _ = close_window(&mut app, id); // 1 回目: 断る
        flush_saves(&mut app);
        let _ = close_window(&mut app, id); // 2 回目: 退避して閉じる
        flush_saves(&mut app);

        // topics は書けないので、vault ルートへ落ちているはず。
        let rescued: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rescue"))
            .collect();
        assert_eq!(rescued.len(), 1, "本文が退避されていない");
        let body = std::fs::read_to_string(&rescued[0]).unwrap();
        assert!(body.contains('X'), "退避したのに編集内容が入っていない");

        // `.rescue` は `.md` ではないので、一覧には出てこない（vault を汚さない）。
        assert!(
            vault::load_dir(&dir)
                .notes
                .iter()
                .all(|n| n.path != rescued[0]),
            "退避ファイルが一覧に出ている"
        );

    }

    /// **保存が通ったら「一度断った」記憶を畳むこと。**
    ///
    /// 立てっぱなしだと、問題が直ったあとに別の失敗を踏んだとき、**警告なしで**
    /// 1 回目の要求がそのまま退避＋終了になる。2 段階にした意味が消える。
    #[test]
    fn a_successful_save_resets_the_refusal() {

        let (dir, mut app) = app_with_folders("close-reset", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _ = close_window(&mut app, iced::window::Id::unique());
        flush_saves(&mut app);
        assert!(app.close_refused, "1 回目を断った記録が残っていない");

        // 権限を直して保存が通れば、記憶は畳まれる。
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);

        assert_eq!(app.saves, 1, "権限を戻したのに保存されていない");
        assert!(!app.close_refused, "保存が通ったのに断った記憶が残っている");
    }

    /// **退避にも失敗したら閉じないこと。** ADR-0007 の「2 回目は必ず閉じる」は
    /// 退避が成功する前提の話で、通常保存も退避先も全滅したときに閉じると、
    /// 唯一残っていたメモリ上の本文を行き先が無いまま捨てることになる。
    ///
    /// Task は中身を覗けないので、「閉じなかった」ことは踏みとどまった痕跡
    /// （本文がエディタに残る・自力退避の手段が画面に出る）で確かめる。
    #[test]
    fn a_failed_rescue_keeps_the_window_open() {

        let (dir, mut app) = app_with_folders("rescue-fail", &[("topics", "消えては困る")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        // 退避先を**全部**書けなくする（容量ゼロで全滅した状況の代役）。
        let locked = dir.join("退避先");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let id = iced::window::Id::unique();
        let _ = rescue_and_close(&mut app, id, "メモ.md", std::slice::from_ref(&locked));

        assert!(
            app.content.text().contains('X'),
            "退避できていないのにエディタから本文が消えた"
        );
        assert!(app.show_marker, "閉じられない理由が画面に出ていない");
        let error = app.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("コピー"),
            "自力で退避する手段が伝わっていない: {error}"
        );

    }

    /// **保存が失敗している間、tick のたびに再試行しないこと。**
    ///
    /// 失敗しても `dirty_since` も `last_edit` も動かないので、バックオフが無いと
    /// `should_save` は以後永久に真になる。100ms ごとに UI スレッドで
    /// 「一時ファイル作成 → 書き込み → `sync_all`」を回すことになり、
    /// **保存できない環境ほど画面が固まって、本文をコピーして逃がすことすらできなくなる。**
    #[test]
    fn a_failing_autosave_backs_off_instead_of_retrying_every_tick() {

        let (dir, mut app) = app_with_folders("retry-backoff", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        let edited = Instant::now();
        let first_try = edited + AUTOSAVE_DEBOUNCE;
        send(&mut app, Message::Tick(first_try));
        flush_saves(&mut app);
        assert_eq!(app.save_failures, 1, "失敗が数えられていない");
        let retry_at = app.retry_after.expect("次の再試行時刻が決まっていない");

        // 待ち時間の内側では、tick が何回来ても試さない。
        for i in 1..=4 {
            send(&mut app, Message::Tick(first_try + TICK * i));
            assert!(app.saving.is_none(), "バックオフ中に保存を投げている");
        }
        assert_eq!(app.save_failures, 1, "バックオフ中に再試行している");

        // 待ち時間を過ぎたら 1 回だけ試し、次の待ちはさらに伸びる。
        send(&mut app, Message::Tick(retry_at));
        flush_saves(&mut app);
        assert_eq!(app.save_failures, 2, "待ち時間を過ぎても再試行していない");
        assert!(
            app.retry_after.is_some_and(|next| next > retry_at),
            "失敗が続いているのに待ち時間が伸びていない"
        );

        // 本文はどこにも消えていない。
        assert!(is_dirty(&app), "保存できていないのに dirty が畳まれている");
        assert!(app.content.text().contains('X'));

    }

    /// **権限が直ったら、バックオフも一緒に畳まれること。**
    /// 待ちが残ったままだと、直ったあとも最大 30 秒書かれない。
    #[test]
    fn a_successful_save_clears_the_backoff() {

        let (dir, mut app) = app_with_folders("retry-clear", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let edited = Instant::now();
        send(&mut app, Message::Tick(edited + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);
        let retry_at = app.retry_after.expect("失敗したのに待ちが設定されていない");

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        send(&mut app, Message::Tick(retry_at));
        flush_saves(&mut app);

        assert_eq!(app.saves, 1, "権限を戻したのに保存されていない");
        assert_eq!(app.save_failures, 0, "失敗の数が畳まれていない");
        assert!(app.retry_after.is_none(), "待ちが残ったままになっている");
    }

    /// 待ち時間は倍々に伸び、上限で頭打ちになること。
    /// 伸び続けると「直ったのに何分も書かれない」になり、伸びないと UI が固まる。
    #[test]
    fn the_retry_delay_grows_and_then_stops_growing() {
        assert_eq!(save_retry_delay(1), SAVE_RETRY_MIN);
        assert_eq!(save_retry_delay(2), SAVE_RETRY_MIN * 2);
        assert_eq!(save_retry_delay(3), SAVE_RETRY_MIN * 4);
        assert_eq!(save_retry_delay(30), SAVE_RETRY_MAX, "上限で止まっていない");
        // 呼ばれない値でも 0 待ち（＝毎 tick 再試行）にはしない。
        assert_eq!(save_retry_delay(0), SAVE_RETRY_MIN);
    }

    /// バックオフ中は、デバウンスも上限も満たしていても書かないこと（境界の両側）。
    #[test]
    fn a_pending_backoff_holds_the_save_off() {
        let start = Instant::now();
        let now = start + AUTOSAVE_DEBOUNCE;

        assert!(
            should_save(now, start, start, None),
            "前提: バックオフが無ければ書く"
        );
        assert!(
            !should_save(now, start, start, Some(now + Duration::from_millis(1))),
            "待ち時間の内側なのに書こうとしている"
        );
        assert!(
            should_save(now, start, start, Some(now)),
            "待ち時間ちょうどで再開していない"
        );
    }

    /// **未保存マーカーが実際に点灯すること。**
    ///
    /// 以前は `should_save` の `else if` に置かれていて、`DIRTY_MARKER_DELAY` >
    /// `AUTOSAVE_MAX_WAIT` である以上**原理的に到達しなかった**。
    /// 「異常のシグナル」と doc に書かれた安全装置が、異常時に沈黙していた。
    #[test]
    fn dirty_marker_lights_up_when_saving_keeps_failing() {

        let (dir, mut app) = app_with_vault("marker");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        let edited = Instant::now();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // 上限（2.5 秒）は超えたがマーカー（3 秒）には届かない時点。まだ黙っている。
        send(&mut app, Message::Tick(edited + AUTOSAVE_MAX_WAIT));
        assert!(is_dirty(&app), "保存に失敗したのに dirty が畳まれた");
        assert!(!app.show_marker, "マーカーの時間に届く前に点灯した");

        // マーカーの時間を越えたら点灯する。
        send(&mut app, Message::Tick(edited + DIRTY_MARKER_DELAY));
        assert!(app.show_marker, "保存できていないのに「未保存」が出ない");

    }

    /// 正常に保存できているうちはマーカーを出さないこと。
    /// 常時点灯したらシグナルとして役に立たない。
    #[test]
    fn dirty_marker_stays_off_while_saves_succeed() {
        let (_dir, mut app) = app_with_vault("marker-quiet");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        let edited = Instant::now();

        send(&mut app, Message::Tick(edited + DIRTY_MARKER_DELAY));
        flush_saves(&mut app);

        assert_eq!(app.saves, 1);
        assert!(!app.show_marker, "保存できているのに「未保存」が出た");
    }

    /// **捨てる前に書き戻すこと。** `.trash` に残るのが「最後に自動保存された内容」だと、
    /// 誤って消したときに直前に打った分だけが戻せない。
    #[test]
    fn deleting_saves_the_pending_edits_into_trash() {
        let (dir, mut app) = app_with_folders("delete-saves", &[("topics", "消すほう")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        send(&mut app, Message::DeleteNote);
        flush_saves(&mut app);

        assert_eq!(app.notes.len(), 0, "一覧から消えていない");
        let trashed: Vec<_> = std::fs::read_dir(dir.join(".trash"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(trashed.len(), 1, ".trash に退避されていない");
        let body = std::fs::read_to_string(trashed[0].path()).unwrap();
        assert!(body.contains('X'), "消す直前の編集が .trash に残っていない");
    }

    /// 打鍵した直後は書かない。1 文字ごとにディスクへ行ったら意味がない。
    #[test]
    fn does_not_save_right_after_a_keystroke() {
        let now = Instant::now();
        assert!(!should_save(now, now, now, None));
    }

    /// 手を止めて 1 秒で書く（デバウンス）。
    #[test]
    fn saves_after_debounce() {
        let start = Instant::now();
        let now = start + AUTOSAVE_DEBOUNCE;
        assert!(should_save(now, start, start, None));
    }

    /// **打ち続けていても上限で書く。** ここが無いと、打鍵が止まらない限り永久に保存されない。
    /// 最後の打鍵は 1ms 前（デバウンスは未達）でも、dirty から 2.5 秒経っていれば書く。
    #[test]
    fn saves_at_max_wait_even_while_typing() {
        let dirty_since = Instant::now();
        let now = dirty_since + AUTOSAVE_MAX_WAIT;
        let last_edit = now - Duration::from_millis(1);
        assert!(should_save(now, last_edit, dirty_since, None));
    }

    /// 上限に達する寸前・打鍵直後なら、まだ書かない（境界の下側）。
    #[test]
    fn holds_off_just_before_max_wait() {
        let dirty_since = Instant::now();
        let now = dirty_since + AUTOSAVE_MAX_WAIT - Duration::from_millis(1);
        let last_edit = now - Duration::from_millis(1);
        assert!(!should_save(now, last_edit, dirty_since, None));
    }

    /// 新規ノートは**開いているノートと同じフォルダ**に作られ、一覧の先頭に出ること。
    #[test]
    fn new_note_lands_next_to_the_open_note_and_appears_first() {
        let (_dir, mut app) =
            app_with_folders("new-note", &[("topics", "設計メモ"), ("notes", "走り書き")]);

        // notes フォルダのノートを開いてから作る。
        let open = app
            .notes
            .iter()
            .position(|n| n.folder == "notes")
            .expect("notes のノートが無い");
        send(&mut app, Message::NoteSelected(open));
        send(&mut app, Message::NewNote);

        let created = app.selected.expect("作ったノートが開かれていない");
        assert_eq!(created, 0, "新規ノートが一覧の先頭に来ていない");
        assert_eq!(app.notes[0].folder, "notes", "開いていたノートと別のフォルダに作られた");
        assert!(app.notes[0].path.exists(), "ファイルが作られていない");
        assert!(app.visible.contains(&0), "作ったノートが一覧に見えていない");
    }

    /// **新規ノートは `0600` で生まれ、保存を重ねても緩まないこと。**
    ///
    /// `save()` は元ファイルの権限を引き継ぐ（ADR-0011）ので、作成時に付いた権限が
    /// そのノートの公開範囲を決め切る。予約を umask 任せにしていた頃は、既存ノートを
    /// `0600` に保つ修正を入れてもなお、**新しく書いたメモだけが `0644`** で並んでいた。
    #[test]
    fn a_new_note_is_created_private_and_stays_private() {

        let (_dir, mut app) = app_with_folders("new-note-perms", &[("notes", "走り書き")]);
        send(&mut app, Message::NewNote);
        let path = app.notes[0].path.clone();

        let created = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(created, 0o600, "新規ノートが既定の umask で作られた");

        send(&mut app, insert('秘'));
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);
        assert_eq!(app.saves, 1, "保存されていない");

        let saved = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(saved, 0o600, "保存で新規ノートの権限が緩んだ");
    }

    /// **作成したらサイドバーのフォルダ件数も増えること。**
    ///
    /// `folders` は起動時に 1 回数えるだけだったので、作っても数字が動かなかった。
    /// `notes` から導かれる状態を更新し忘れる類の食い違いなので、回帰テストとして残す。
    #[test]
    fn creating_a_note_updates_the_folder_count() {
        let (_dir, mut app) =
            app_with_folders("folder-count", &[("notes", "あ"), ("notes", "い")]);
        let before = app
            .folders
            .iter()
            .find(|(name, _)| name == "notes")
            .map(|(_, count)| *count)
            .expect("notes フォルダが無い");
        assert_eq!(before, 2);

        send(&mut app, Message::FolderSelected(Some("notes".to_string())));
        send(&mut app, Message::NewNote);

        let after = app
            .folders
            .iter()
            .find(|(name, _)| name == "notes")
            .map(|(_, count)| *count)
            .expect("notes フォルダが消えた");
        assert_eq!(after, before + 1, "サイドバーの件数が増えていない");
    }

    /// **先頭への差し込みで既存インデックスがずれても、開いているノートがすり替わらないこと。**
    ///
    /// `selected` の付け替えを忘れると、作成した瞬間に隣のノートを編集し始める。
    /// 再現条件が分かりにくいので回帰テストとして残す。
    #[test]
    fn creating_a_note_does_not_swap_the_previously_open_note() {
        let (_dir, mut app) =
            app_with_folders("reindex", &[("topics", "あ"), ("topics", "い"), ("topics", "う")]);
        send(&mut app, Message::NoteSelected(2));
        let before = app.notes[2].title.clone();

        // 作った直後は新規ノートが開くので、元のノートへ戻って同じものかを見る。
        send(&mut app, Message::NewNote);
        let moved = app
            .notes
            .iter()
            .position(|n| n.title == before)
            .expect("元のノートが消えた");
        send(&mut app, Message::NoteSelected(moved));

        assert_eq!(app.notes[app.selected.unwrap()].title, before);
        assert!(
            app.content.text().contains("本文"),
            "別のノートの本文が開いている"
        );
    }

    /// 同一秒に連打しても両方残ること（ファイル名は秒精度）。
    #[test]
    fn creating_twice_in_the_same_second_keeps_both() {
        let (_dir, mut app) = app_with_folders("same-second", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));

        send(&mut app, Message::NewNote);
        let first = app.notes[0].path.clone();
        send(&mut app, Message::NewNote);
        let second = app.notes[0].path.clone();

        assert_ne!(first, second, "同じパスを 2 回使っている");
        assert!(first.exists() && second.exists(), "先に作ったファイルが消えた");
        assert_eq!(app.notes.len(), 3);
    }

    /// **保存に失敗したら新規作成もしないこと。** 作成もエディタを上書きする操作。
    #[test]
    fn new_note_is_blocked_when_saving_fails() {

        let (dir, mut app) = app_with_folders("new-note-blocked", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        send(&mut app, Message::NewNote);
        flush_saves(&mut app);

        assert_eq!(app.notes.len(), 1, "保存に失敗したのにノートが増えた");
        assert!(app.content.text().contains('X'), "編集内容が消えた");
        assert!(app.error.is_some());

    }

    /// **絞り込み中は、開いているノートのフォルダより絞り込みが優先されること。**
    ///
    /// 逆にすると「`notes` を見ているのに `topics` に作られ、それを見せるために
    /// 一覧がすべてに戻る」という動きになる（実際に触って直した）。
    #[test]
    fn creating_while_filtered_uses_that_folder_and_keeps_the_filter() {
        let (_dir, mut app) =
            app_with_folders("filter-wins", &[("topics", "設計メモ"), ("notes", "走り書き")]);

        // topics のノートを開いたまま、notes で絞り込んで作る。
        let topics = app.notes.iter().position(|n| n.folder == "topics").unwrap();
        send(&mut app, Message::NoteSelected(topics));
        send(&mut app, Message::FolderSelected(Some("notes".to_string())));
        send(&mut app, Message::NewNote);

        assert_eq!(app.notes[0].folder, "notes", "見ているフォルダに作られていない");
        assert_eq!(
            app.selected_folder.as_deref(),
            Some("notes"),
            "絞り込みが勝手に解除された"
        );
        assert!(app.visible.contains(&0), "作ったノートが一覧に見えていない");
    }

    /// **一覧から消える名前は拒否し、拡張子は補うこと。**
    ///
    /// dot 始まりは前身で実際に踏んだ（`load_dir()` が走査から除外する）。
    /// 拡張子落ちは**同型の穴**で、`load_dir()` が `.md` しか拾わないため同じく消える。
    #[test]
    fn note_names_that_would_vanish_are_rejected_or_repaired() {
        assert_eq!(validate_note_name("設計メモ").unwrap(), "設計メモ.md");
        assert_eq!(validate_note_name(" 設計メモ.md ").unwrap(), "設計メモ.md");

        assert!(validate_note_name("").is_err(), "空を通した");
        assert!(validate_note_name("   ").is_err(), "空白だけを通した");
        assert!(validate_note_name(".secret").is_err(), "dot 始まりを通した");
        assert!(validate_note_name("a/b.md").is_err(), "/ を通した");
        assert!(validate_note_name("a\\b.md").is_err(), "\\ を通した");
    }

    /// リネームはファイル名だけを変え、本文には触らないこと。
    #[test]
    fn rename_changes_the_file_name_and_keeps_the_body() {
        let (_dir, mut app) = app_with_folders("rename", &[("topics", "設計メモ")]);
        send(&mut app, Message::NoteSelected(0));
        let before = app.notes[0].raw.clone();

        send(&mut app, Message::RenameStarted);
        send(&mut app, Message::RenameChanged("新しい名前".to_string()));
        send(&mut app, Message::RenameCommit);

        assert!(app.rename.is_none(), "リネーム後も入力欄が開いている");
        assert_eq!(
            app.notes[0].path.file_name().unwrap(),
            "新しい名前.md",
            "拡張子が補われていない"
        );
        assert!(app.notes[0].path.exists(), "移動先にファイルが無い");
        assert_eq!(app.notes[0].raw, before, "本文が書き換わった");
        // frontmatter の title: があるので、表示タイトルは変わらないのが仕様。
        assert_eq!(app.notes[0].title, "設計メモ");
    }

    /// **同名が居たら枝番へ逃がし、既存を絶対に潰さないこと。**
    #[test]
    fn rename_into_an_existing_name_gets_a_suffix() {
        let (_dir, mut app) =
            app_with_folders("rename-collision", &[("topics", "先客"), ("topics", "動くほう")]);
        let moving = app.notes.iter().position(|n| n.title == "動くほう").unwrap();
        let victim = app.notes.iter().position(|n| n.title == "先客").unwrap();
        let victim_name = app.notes[victim]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let victim_body = app.notes[victim].raw.clone();

        send(&mut app, Message::NoteSelected(moving));
        send(&mut app, Message::RenameStarted);
        send(&mut app, Message::RenameChanged(victim_name.clone()));
        send(&mut app, Message::RenameCommit);

        assert_eq!(
            app.notes[moving].path.file_name().unwrap(),
            format!("{}-2.md", victim_name.trim_end_matches(".md")).as_str()
        );
        assert_eq!(app.notes[victim].raw, victim_body, "先客が潰された");
        assert!(app.notes[victim].path.exists());
    }

    /// 一覧から消える名前は、エラーを出して**入力欄を開いたまま**にすること。
    /// 閉じてしまうと打ち直せない。
    #[test]
    fn rejected_rename_keeps_the_input_open() {
        let (_dir, mut app) = app_with_folders("rename-rejected", &[("topics", "設計メモ")]);
        send(&mut app, Message::NoteSelected(0));
        let before = app.notes[0].path.clone();

        send(&mut app, Message::RenameStarted);
        send(&mut app, Message::RenameChanged(".secret".to_string()));
        send(&mut app, Message::RenameCommit);

        assert!(app.rename.is_some(), "打ち直せない");
        assert!(app.error.is_some(), "理由が表に出ていない");
        assert_eq!(app.notes[0].path, before, "拒否したのにリネームされた");
    }

    /// subscription が拾う打鍵を組み立てる（`Message::Key` の実経路を通すため）。
    fn pressed(k: keyboard::Key, modifiers: keyboard::Modifiers) -> Message {
        Message::Key(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    /// **リネーム欄を開いたまま `Cmd+N` しても、新規ノートが前の名前でリネームされないこと。**
    ///
    /// `NewNote` はパレットしか畳んでいなかった。入力欄が開いたまま残ると、そこで押した
    /// Enter は `RenameCommit` として届き、対象は `app.selected` = 作られたばかりの
    /// 新規ノートなので、**前のノートのファイル名で誤リネームされる**実害があった。
    #[test]
    fn creating_a_note_closes_an_open_rename() {
        let (_dir, mut app) = app_with_folders("rename-vs-new", &[("topics", "設計メモ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, Message::RenameStarted);

        send(&mut app, Message::NewNote);
        assert!(app.rename.is_none(), "リネーム欄が開いたまま残っている");

        // 万一 Enter がすり抜けても、対象を失った確定は何もしないこと。
        let before = app.notes[0].path.clone();
        send(&mut app, Message::RenameCommit);
        assert_eq!(app.notes[0].path, before, "新規ノートが前の名前でリネームされた");
    }

    /// **リネーム欄を開いたまま一覧の別ノートを選んでも、そのノートがリネームされないこと。**
    #[test]
    fn selecting_a_note_closes_an_open_rename() {
        let (_dir, mut app) =
            app_with_folders("rename-vs-select", &[("topics", "先客"), ("topics", "動くほう")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, Message::RenameStarted);

        send(&mut app, Message::NoteSelected(1));
        assert!(app.rename.is_none(), "リネーム欄が開いたまま残っている");

        let before = app.notes[1].path.clone();
        send(&mut app, Message::RenameCommit);
        assert_eq!(app.notes[1].path, before, "選択先が前の名前でリネームされた");
    }

    /// **リネーム欄を開いたまま `Cmd+P` すると入力欄が畳まれ、パレットの Enter が
    /// 選択として届くこと。**
    ///
    /// 両方開いたままだと `Message::Key` の分岐がリネームの Enter を先に拾い、
    /// パレットで選んだつもりの Enter が `RenameCommit` として走る。
    #[test]
    fn opening_the_palette_closes_an_open_rename() {
        let (_dir, mut app) =
            app_with_folders("rename-vs-palette", &[("topics", "先客"), ("topics", "動くほう")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, Message::RenameStarted);

        send(&mut app, pressed(key("p"), keyboard::Modifiers::COMMAND));
        assert!(app.rename.is_none(), "パレットとリネーム欄が同時に開いている");
        assert!(app.palette.is_some(), "パレットが開いていない");

        let before: Vec<_> = app.notes.iter().map(|n| n.path.clone()).collect();
        send(&mut app, pressed(named(keyboard::key::Named::Enter), keyboard::Modifiers::empty()));
        assert!(app.palette.is_none(), "Enter でパレットから開けていない");
        let after: Vec<_> = app.notes.iter().map(|n| n.path.clone()).collect();
        assert_eq!(after, before, "パレットからの選択でリネームが走った");
    }

    /// サブフォルダが無い vault では `/` の行を出さないこと。
    ///
    /// 出すと「すべて」と必ず同じ件数の行が隣に並び、しかも選んでも絞り込みが起きない。
    /// サブフォルダが 1 つでもあれば `/` は「直下だけ見る」という意味を持つので出す。
    #[test]
    fn the_root_row_is_hidden_until_a_subfolder_exists() {
        let (_dir, app) = app_with_folders("root-only", &[("", "あ"), ("", "い")]);
        assert_eq!(app.notes.len(), 2);
        assert!(
            app.folders.is_empty(),
            "ルート直下だけなのに行が出ている: {:?}",
            app.folders
        );

        let (_dir, app) = app_with_folders("root-and-sub", &[("", "あ"), ("topics", "い")]);
        let names: Vec<&str> = app.folders.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&vault::ROOT_FOLDER) && names.contains(&"topics"),
            "サブフォルダがあるときは / も出す: {names:?}"
        );
    }

    /// 削除は `.trash` へ退避し、一覧とフォルダ件数から消えること。ファイルは残ること。
    #[test]
    fn delete_moves_the_note_to_trash() {
        let (dir, mut app) =
            app_with_folders("delete", &[("topics", "残るほう"), ("topics", "消すほう")]);
        let target = app.notes.iter().position(|n| n.title == "消すほう").unwrap();
        send(&mut app, Message::NoteSelected(target));

        send(&mut app, Message::DeleteNote);

        assert_eq!(app.notes.len(), 1, "一覧から消えていない");
        assert_eq!(app.notes[0].title, "残るほう");
        // ADR-0022: 消した位置に選択が残り、繰り上がったノートが開く。
        assert_eq!(app.selected, Some(0), "消したあとに残ったノートを開いていない");
        assert!(
            app.content.text().contains("残るほう"),
            "残ったノートを開き直していない"
        );
        assert_eq!(
            app.folders.iter().find(|(n, _)| n == "topics").unwrap().1,
            1,
            "フォルダ件数が減っていない"
        );

        // ファイルとしては .trash に残っている（Finder で戻せる）。
        let trashed: Vec<_> = std::fs::read_dir(dir.join(".trash")).unwrap().collect();
        assert_eq!(trashed.len(), 1, ".trash に退避されていない");
    }

    /// 削除しても選択は**消した位置に居続ける**こと。末尾を消したときだけ 1 つ前へ寄る
    /// （ADR-0022）。`Cmd+Delete` を連打したときに視点が飛ばないための規則。
    #[test]
    fn deleting_keeps_the_selection_at_the_same_position() {
        let (_dir, mut app) =
            app_with_folders("delete-keeps", &[("", "一"), ("", "二"), ("", "三")]);
        let below = app.notes[2].title.clone();

        send(&mut app, Message::NoteSelected(1));
        send(&mut app, Message::DeleteNote);

        assert_eq!(app.selected, Some(1), "消した位置に選択が残っていない");
        assert_eq!(
            app.notes[1].title, below,
            "繰り上がってきたノートを開いていない"
        );
        assert!(
            app.content.text().contains(below.as_str()),
            "エディタが空のまま（開き直していない）"
        );

        // 末尾（残り 2 件の index 1）を消したら 1 つ前へ寄る。
        send(&mut app, Message::NoteSelected(1));
        send(&mut app, Message::DeleteNote);

        assert_eq!(app.selected, Some(0), "末尾を消したのに 1 つ前へ寄っていない");
    }

    /// 最後の 1 件を消したときだけ未選択に戻ること（ADR-0022 の不変条件
    /// 「未選択 ⟺ ノートが 0 件」の片側）。エディタには前の本文を残さない。
    #[test]
    fn deleting_the_last_note_leaves_nothing_selected() {
        let (_dir, mut app) = app_with_folders("delete-last", &[("", "ひとつ")]);
        assert_eq!(app.selected, Some(0), "起動時に開いていない");

        send(&mut app, Message::DeleteNote);

        assert!(app.notes.is_empty(), "一覧から消えていない");
        assert_eq!(app.selected, None, "0 件なのに選択が残っている");
        assert_eq!(app.content.text(), "", "消したノートの本文がエディタに残っている");
    }

    /// **起動したら直近に更新されたノートが開いていること**（ADR-0022 の柱 1）。
    ///
    /// これが無いと、未選択のエディタに打った本文がノートを開いた瞬間に無警告で消える
    /// （ADR-0002 が廃止した「打てるのに保存されない」経路と同型）。
    #[test]
    fn booting_opens_the_newest_note() {
        let (_dir, app) = app_with_vault("boot-newest");

        assert_eq!(app.selected, Some(0), "起動直後にノートが開いていない");
        assert_eq!(app.notes[0].title, "新しい", "一覧の先頭が直近のノートでない");
        assert!(
            app.content.text().contains("新しい"),
            "エディタに先頭ノートの本文が入っていない"
        );
    }

    /// **起動時に開いてもディスクには書かないこと。** Done の定義に明記された禁止事項で、
    /// ここが破れると起動しただけで vault 全体の mtime が動きうる。
    #[test]
    fn booting_does_not_write_the_note_it_opens() {
        let (_dir, mut app) = app_with_vault("boot-no-write");
        let before = mtime(&app.notes[0].path);

        // デバウンスを跨いでも、開いただけなら書かない。
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);

        assert_eq!(app.saves, 0, "起動時に開いただけで書き込んでいる");
        assert_eq!(mtime(&app.notes[0].path), before, "開いただけで mtime が動いた");
    }

    /// 0 件の vault では未選択のままで、エディタの中身も空であること（ADR-0022）。
    /// このときだけ画面にエディタが出ない（`empty_pane`）。
    #[test]
    fn booting_an_empty_vault_selects_nothing() {
        let dir = TempDir::new("boot-empty");
        let app = boot(dir.to_path_buf(), vault::load_dir(&dir), 0.0);

        assert!(app.notes.is_empty(), "空の vault を作れていない");
        assert_eq!(app.selected, None, "0 件なのに選択がある");
        assert_eq!(
            app.content.text(),
            "",
            "空 vault なのにエディタへ文字が入っている（打てば消える経路）"
        );

        // `empty_pane` の枝を実際に通す。**画面に何が出ているかはテストから見えない**ので、
        // ここで確かめられるのは「エディタを出さない枝が組み立つ」ことだけ（目視は別途）。
        drop(view(&app));
    }

    /// **実データで `view()` の構築時間を測る。** Done の定義は打鍵時 1ms 未満。
    ///
    /// 実データが要るので既定では走らせない（環境依存のテストを CI に混ぜない）。
    ///
    /// 実行: `VAULT="$HOME/..." cargo test --release -- --ignored --nocapture`
    /// **必ず --release で。** debug の数字は判断材料にならない。
    #[test]
    #[ignore = "実データの vault が要る。VAULT を指定して --ignored で走らせる"]
    fn measure_view_construction_with_real_vault() {
        // このテストは実データ専用なので、記憶した保存先ではなく `VAULT` だけを見る。
        let root = real_vault_root();
        let load = vault::load_dir(&root);
        let count = load.notes.len();
        let mut app = boot(root, load, 0.0);

        // 一番大きいノートを開く。エディタの負荷が最大になる条件で測る。
        let biggest = app
            .notes
            .iter()
            .enumerate()
            .max_by_key(|(_, note)| note.raw.len())
            .map(|(i, _)| i)
            .expect("vault が空");
        send(&mut app, Message::NoteSelected(biggest));

        let measure = |app: &App, label: &str| {
            const RUNS: usize = 20;
            let mut worst = 0.0_f64;
            let mut total = 0.0_f64;
            for _ in 0..RUNS {
                let t0 = Instant::now();
                let element = view(app);
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                drop(element);
                worst = worst.max(ms);
                total += ms;
            }
            let avg = total / RUNS as f64;
            println!("  {label}: 平均 {avg:.3}ms / 最悪 {worst:.3}ms");
            worst
        };

        println!("vault: {count} notes / 開いたノート {} 文字", app.notes[biggest].raw.len());
        let plain = measure(&app, "一覧のみ      ");
        open_palette(&mut app, "");
        let with_palette = measure(&app, "パレット表示中");

        assert!(
            plain < 1.0 && with_palette < 1.0,
            "view() の構築が 1ms を超えた（Done の定義違反）"
        );
    }

    /// 実測用の vault。**記憶した保存先ではなく `VAULT` だけを見る**
    /// （実データ専用の計測を、うっかり普段の vault で走らせない）。
    fn real_vault_root() -> PathBuf {
        match resolve_vault(std::env::var("VAULT").ok(), None, PathBuf::new()) {
            VaultChoice::Ready(root) => root,
            other => panic!("VAULT に実データの vault を指定して実行する: {other:?}"),
        }
    }

    /// 打鍵 1 回ぶんの `refilter` を測って**平均**（ms）を返す。
    ///
    /// 予算を平均で見るのは、これが打鍵のたびに走るものだから。体感を決めるのは
    /// 連続した打鍵の均しであって、他プロセスやサーマルの影響を拾った 1 回ではない。
    /// 最悪値は記録として出すだけで、判定には使わない。
    fn measure_refilter(notes: &[vault::Note], label: &str, query: &str) -> f64 {
        const RUNS: usize = 20;
        // 1 回捨てる。初回はページフォルトとキャッシュミスを含み、
        // 「打鍵のたびに走る」という実態を表さない。
        drop(refilter(notes, query));

        let mut worst = 0.0_f64;
        let mut total = 0.0_f64;
        let mut hits = 0;
        for _ in 0..RUNS {
            let t0 = Instant::now();
            let result = refilter(notes, query);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            hits = result.len();
            drop(result);
            worst = worst.max(ms);
            total += ms;
        }
        let avg = total / RUNS as f64;
        println!("  {label}: 平均 {avg:.3}ms / 最悪 {worst:.3}ms（{hits} 件）");
        avg
    }

    /// 実データの分布に寄せた合成 vault。**乱数は使わない**（数字が揺れると比較できない）。
    fn synthetic_notes(count: usize) -> Vec<vault::Note> {
        // 実データの vault は日本語主体で、コードブロックが混じる。
        let paragraphs = [
            "iced の text_editor は Content を持ち、view() では borrow するだけで済む。",
            "保存は fsync してからアトミックに rename する。途中で落ちても壊れたファイルは残らない。",
            "```rust\nfn main() {\n    println!(\"hello\");\n}\n```",
            "frontmatter の tags は [a, b] とブロック形式の両方が現場に存在している。",
            "パレットの候補リストにはスクロール追従が要る。選択だけ動いても画面は動かない。",
        ];

        let mut notes: Vec<vault::Note> = (0..count)
            .map(|i| {
                let mut body = format!("---\ntitle: \"メモ {i}\"\ntags: [rust, iced]\n---\n\n");
                // 1 件あたり約 3KB。並べ方をノートごとにずらして同一内容を避ける。
                for n in 0..24 {
                    body.push_str(paragraphs[(i + n) % paragraphs.len()]);
                    body.push_str("\n\n");
                }
                note_with(&format!("メモ {i}"), &body)
            })
            .collect();

        // 実データの最大ノートに合わせて 1 件だけ極端に大きくする
        // （212,564 文字 = docs/implementation-notes.md の実測値）。
        let unit = paragraphs.join("\n\n");
        let reps = 212_564 / unit.chars().count() + 1;
        notes[0] = note_with("大きいノート", &unit.repeat(reps));

        // 1 件にだけ当たる語。「絞り込めているのに速い」ことを確かめるため。
        notes[count / 2].raw.push_str("\n特異な合言葉ヰヱヲン\n");
        notes
    }

    /// **打鍵 1 回の `refilter` が予算内に収まること。** 予算は最悪 8ms（spec）。
    ///
    /// 約 1200 件級の実データ vault が手元に無くても回せるよう、コーパスを合成する。
    /// ディスクには何も書かない。
    ///
    /// 実行: `cargo test --release -- --ignored --nocapture`
    /// **必ず --release で。** debug の数字は判断材料にならない。
    #[test]
    #[ignore = "時間を測るテスト。--release で --ignored 指定のときだけ走らせる"]
    fn measure_refilter_with_synthetic_vault() {
        let notes = synthetic_notes(1200);
        let bytes: usize = notes.iter().map(|n| n.raw.len()).sum();
        println!(
            "合成 vault: {} 件 / 本文 {:.1}MB（最大 {} 文字）",
            notes.len(),
            bytes as f64 / 1_048_576.0,
            notes[0].raw.chars().count()
        );

        let mut slowest = 0.0_f64;
        // 空クエリと 1 文字は本文を読まない経路。ここが遅いと全部が遅い。
        slowest = slowest.max(measure_refilter(&notes, "空クエリ          ", ""));
        slowest = slowest.max(measure_refilter(&notes, "1 文字            ", "い"));
        // 大量に当たるクエリ。早期打ち切りが効くので速いはず。
        slowest = slowest.max(measure_refilter(&notes, "大量に当たる      ", "iced"));
        // 1 件だけに当たる。全件走査するが結果は絞れている。
        slowest = slowest.max(measure_refilter(&notes, "1 件だけに当たる  ", "合言葉ヰヱヲン"));
        // **最悪ケース: どこにも当たらないクエリ。** 打鍵途中の未完成語がこれになる。
        slowest = slowest.max(measure_refilter(&notes, "どこにも当たらない", "ｑｚｘ在存"));

        // 実使用の形。1 文字ずつ伸ばしていく。
        println!("  --- 打鍵列 ---");
        for len in 1..="scrollable".len() {
            let query = &"scrollable"[..len];
            slowest = slowest.max(measure_refilter(&notes, &format!("  {query:<12}"), query));
        }

        assert!(
            slowest < 8.0,
            "refilter の平均が予算 8ms を超えた: {slowest:.3}ms"
        );
    }

    /// 同じ計測を実データで。合成コーパスの数字を答え合わせするためのもの。
    ///
    /// 実行: `VAULT="$HOME/..." cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore = "実データの vault が要る。VAULT を指定して --ignored で走らせる"]
    fn measure_refilter_with_real_vault() {
        let root = real_vault_root();
        let notes = vault::load_dir(&root).notes;
        let bytes: usize = notes.iter().map(|n| n.raw.len()).sum();
        println!(
            "vault: {} 件 / 本文 {:.1}MB",
            notes.len(),
            bytes as f64 / 1_048_576.0
        );

        let mut slowest = 0.0_f64;
        slowest = slowest.max(measure_refilter(&notes, "空クエリ          ", ""));
        slowest = slowest.max(measure_refilter(&notes, "どこにも当たらない", "ｑｚｘ在存"));
        slowest = slowest.max(measure_refilter(&notes, "よくある語        ", "した"));

        assert!(
            slowest < 8.0,
            "refilter の平均が予算 8ms を超えた: {slowest:.3}ms"
        );
    }

    /// `refilter` だけを見たいので、ファイルシステムを触らずに Note を組み立てる。
    /// `app_with_folders` は本文を固定で作るため、本文検索の確認には使えない。
    fn note_with(title: &str, body: &str) -> vault::Note {
        vault::Note {
            path: PathBuf::from(format!("{title}.md")),
            title: title.to_string(),
            tags: Vec::new(),
            folder: "/".to_string(),
            preview: String::new(),
            raw: body.to_string(),
            modified: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    /// **タイトルに無い語でも本文で辿り着けること。** この機能の眼目。
    #[test]
    fn palette_finds_a_note_by_its_body() {
        let notes = vec![
            note_with("設計メモ", "本文に scrollable のことを書いた"),
            note_with("買い物", "牛乳とパン"),
        ];
        let hits = refilter(&notes, "scrollable");

        assert_eq!(hits.len(), 1);
        assert_eq!(notes[hits[0].note].title, "設計メモ");
        assert!(matches!(hits[0].kind, HitKind::Body { .. }));
    }

    /// タイトル一致は本文一致より必ず上に出ること。
    #[test]
    fn title_hits_come_before_body_hits() {
        let notes = vec![
            note_with("買い物", "そういえば iced のことも書いた"),
            note_with("iced のメモ", "本文には書いていない"),
        ];
        let hits = refilter(&notes, "iced");

        assert_eq!(hits.len(), 2);
        assert_eq!(
            notes[hits[0].note].title, "iced のメモ",
            "タイトル一致が先頭に来ていない"
        );
        assert!(matches!(hits[0].kind, HitKind::Title(_)));
        assert!(matches!(hits[1].kind, HitKind::Body { .. }));
    }

    /// タイトルにも本文にも当たるノートを二度出さないこと。
    #[test]
    fn a_note_is_listed_once_when_both_match() {
        let notes = vec![note_with("iced のメモ", "本文にも iced と書いてある")];
        let hits = refilter(&notes, "iced");

        assert_eq!(hits.len(), 1, "同じノートが 2 行に出ている");
        assert!(
            matches!(hits[0].kind, HitKind::Title(_)),
            "タイトル一致が優先されていない"
        );
    }

    /// **タイトルだけで枠が埋まったら本文を 1 バイトも読まないこと。**
    /// 打ち切りをソートの後に置いていた頃は、ここが走査コストの削減にならなかった。
    #[test]
    fn body_is_not_searched_when_the_title_hits_fill_the_list() {
        let mut notes: Vec<vault::Note> = (0..PALETTE_MAX_RESULTS + 5)
            .map(|i| note_with(&format!("aa-{i}"), "本文は関係ない"))
            .collect();
        // タイトルには当たらず本文にだけ当たるノート。枠が無いので出てはいけない。
        notes.push(note_with("ほか", "ここに aa がある"));

        let hits = refilter(&notes, "aa");

        assert_eq!(hits.len(), PALETTE_MAX_RESULTS);
        assert!(
            hits.iter().all(|h| matches!(h.kind, HitKind::Title(_))),
            "枠が埋まっているのに本文ヒットが混じっている"
        );
    }

    /// 1 文字では本文を探さないこと。ほぼ全件に当たって情報量がゼロなため。
    #[test]
    fn a_single_character_query_does_not_search_bodies() {
        let notes = vec![note_with("メモ", "本文に z がある")];
        assert!(refilter(&notes, "z").is_empty(), "1 文字で本文を探している");

        let notes = vec![note_with("メモ", "本文に zz がある")];
        assert_eq!(refilter(&notes, "zz").len(), 1, "2 文字なら探すこと");
    }

    /// 本文ヒットは一覧と同じ順（`notes` の順 = 更新の新しい順）に並ぶこと。
    #[test]
    fn body_hits_keep_the_note_list_order() {
        let notes = vec![
            note_with("あ", "zzz は先頭のノートにある"),
            note_with("い", "zzz は 2 番目にもある"),
            note_with("う", "zzz は 3 番目にも"),
        ];
        let hits = refilter(&notes, "zzz");

        let order: Vec<usize> = hits.iter().map(|h| h.note).collect();
        assert_eq!(order, [0, 1, 2]);
    }

    /// 上位 `PALETTE_MAX_RESULTS` 件で打ち切ること。約 1200 件を全部描いても人は読まない。
    #[test]
    fn refilter_truncates_to_the_max_results() {
        let entries: Vec<(String, String)> = (0..PALETTE_MAX_RESULTS + 10)
            .map(|i| ("topics".to_string(), format!("メモ {i}")))
            .collect();
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(f, t)| (f.as_str(), t.as_str()))
            .collect();
        let (_dir, app) = app_with_folders("truncate", &refs);

        assert_eq!(refilter(&app.notes, "").len(), PALETTE_MAX_RESULTS);
    }

    /// **パレットは絞り込み中のフォルダを無視して全ノートを探し、開いたら絞り込みを解除すること。**
    ///
    /// 解除しないと「選択中のノートが左の一覧に無い」状態が生まれる。
    #[test]
    fn palette_opens_a_note_from_another_folder_and_clears_the_filter() {
        let (_dir, mut app) =
            app_with_folders("cross-folder", &[("topics", "設計メモ"), ("notes", "走り書き")]);

        send(&mut app, Message::FolderSelected(Some("notes".to_string())));
        assert_eq!(app.visible.len(), 1, "フォルダ絞り込みが効いていない");

        // 絞り込み対象外（topics）のノートを検索して開く。
        open_palette(&mut app, "設計");
        assert_eq!(app.palette.as_ref().unwrap().matches.len(), 1);
        let _ = handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());

        let opened = app.selected.expect("ノートが開かれていない");
        assert_eq!(app.notes[opened].title, "設計メモ");
        assert!(app.palette.is_none(), "開いたのにパレットが残っている");
        assert!(app.selected_folder.is_none(), "絞り込みが解除されていない");
        assert!(
            app.visible.contains(&opened),
            "開いたノートが一覧に見えていない"
        );
    }

    /// **絞り込みの中のノートをパレットから開いたときは、絞り込みを維持すること。**
    ///
    /// 以前は無条件に解除していて「topics で絞り込んで topics のノートを開いたのに
    /// すべてに戻る」動きになっていた。解除は「開いたノートが一覧から消える」ときだけの手段。
    #[test]
    fn palette_keeps_the_filter_when_the_note_is_already_visible() {
        let (_dir, mut app) = app_with_folders(
            "palette-keeps-filter",
            &[("topics", "設計メモ"), ("topics", "別の設計"), ("notes", "走り書き")],
        );
        send(&mut app, Message::FolderSelected(Some("topics".to_string())));

        open_palette(&mut app, "設計メモ");
        let _ = handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());

        assert_eq!(app.notes[app.selected.unwrap()].title, "設計メモ");
        assert_eq!(
            app.selected_folder.as_deref(),
            Some("topics"),
            "同じフォルダのノートを開いただけで絞り込みが解除された"
        );
    }

    /// **保存に失敗したらパレットからも遷移しないこと。** 一覧クリックと同じガードが要る。
    /// パレットは閉じない（閉じてから中断すると、なぜ切り替わらないのかが分からなくなる）。
    #[test]
    fn palette_enter_is_blocked_when_saving_fails() {

        let (dir, mut app) =
            app_with_folders("palette-save-fails", &[("topics", "あ"), ("topics", "い")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        open_palette(&mut app, "");
        // 先頭以外を選んでから Enter（自分自身を開き直すのでは検証にならない）。
        app.palette.as_mut().unwrap().selected = 1;
        let _ = handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());
        flush_saves(&mut app);

        assert_eq!(app.selected, Some(0), "保存に失敗したのに切り替わった");
        assert!(app.palette.is_some(), "中断したのにパレットが閉じた");
        assert!(app.error.is_some(), "保存失敗が表に出ていない");
        assert!(app.content.text().contains('X'), "編集内容が消えた");

    }

    /// ctrl-n / ctrl-p が循環すること。矢印キーは `text_input` に消費されて届かない。
    #[test]
    fn palette_selection_cycles_with_ctrl_n_and_ctrl_p() {
        let (_dir, mut app) =
            app_with_folders("cycle", &[("topics", "あ"), ("topics", "い"), ("topics", "う")]);
        open_palette(&mut app, "");
        assert_eq!(app.palette.as_ref().unwrap().matches.len(), 3);

        let ctrl = keyboard::Modifiers::CTRL;
        let _ = handle_palette_key(&mut app, &key("n"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 1);
        let _ = handle_palette_key(&mut app, &key("p"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 0);
        // 先頭で戻ると末尾へ回り込む。
        let _ = handle_palette_key(&mut app, &key("p"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 2);
    }

    /// frontmatter のタグが `#` 付きの 1 行になること。
    ///
    /// 一覧の行とパレットの行はどちらもこの文字列を出すので、
    /// 「タグを設定しても画面に出ない」の回帰はここで止まる。
    #[test]
    fn tags_render_as_a_single_hashed_line() {
        assert_eq!(
            tag_label(&["rust".to_string(), "iced".to_string()]),
            "#rust #iced"
        );
        assert_eq!(tag_label(&[]), "");
    }

    /// 選択位置が候補リストのスクロール位置に変換されること。
    ///
    /// ここが常に 0 だと、ハイライトだけが可視領域（360px ≒ 8 行）の外へ出て
    /// 「選択は動いているのに画面では見えない」状態になる（実際にそうなっていた）。
    #[test]
    fn palette_scroll_offset_reaches_both_ends() {
        assert_eq!(palette_scroll_offset(0, 50), 0.0);
        // 末尾を選んだら最下部。相対で出すので 360px が行の高さの整数倍でなくても、
        // 最後の 1 件が切れ残らない。
        assert_eq!(palette_scroll_offset(49, 50), 1.0);
        assert_eq!(palette_scroll_offset(24, 49), 0.5);
        // 候補が 0 件・1 件でもゼロ除算しない。
        assert_eq!(palette_scroll_offset(0, 0), 0.0);
        assert_eq!(palette_scroll_offset(0, 1), 0.0);
    }

    /// Escape で閉じること（`Cmd+P` のトグルが使えないので、閉じ方はこちらに寄せている）。
    #[test]
    fn palette_closes_on_escape() {
        let (_dir, mut app) = app_with_folders("escape", &[("topics", "あ")]);
        open_palette(&mut app, "");
        let _ = handle_palette_key(&mut app, &named(keyboard::key::Named::Escape), <_>::default());
        assert!(app.palette.is_none());
    }

    /// `text_editor` に届く打鍵を組み立てる。
    ///
    /// **`text` は `Cmd` を押していても付いてくる。** iced はこれを見て `Insert` を作るので、
    /// ここが混入の入口になる。だから `Cmd+P` の再現には `text: Some("p")` が要る。
    /// エディタへ直接渡す打鍵。`text` は macOS の実機が制御文字を載せてくる経路と、
    /// 何も載らない経路の両方を作り分けられるように引数で受ける。
    fn press(c: &str, modifiers: keyboard::Modifiers, text: Option<&str>) -> text_editor::KeyPress {
        text_editor::KeyPress {
            key: key(c),
            modified_key: key(c),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            modifiers,
            text: text.map(Into::into),
            status: text_editor::Status::Focused { is_hovered: false },
        }
    }

    fn key_press(c: &str, modifiers: keyboard::Modifiers) -> text_editor::KeyPress {
        press(c, modifiers, Some(c))
    }

    /// **`Cmd+P` の "p" が本文に入らないこと。** 実 vault のノートに "p" が入って
    /// 自動保存されるところまで実際に踏んだ回帰。ショートカットは subscription で拾うが、
    /// subscription はイベントを消費できないので、同じ打鍵がここにも届く（ADR-0010）。
    #[test]
    fn command_shortcuts_do_not_reach_the_body() {
        for c in ["p", "n", "r"] {
            assert!(
                editor_key_binding(key_press(c, keyboard::Modifiers::COMMAND)).is_none(),
                "Cmd+{c} が本文に入る",
            );
        }
    }

    /// `Ctrl` 単独の打鍵を組み立てる。
    ///
    /// **`text` には制御文字が入る。** macOS の winit は `text_with_all_modifiers()` を返すので、
    /// `Ctrl+A` は `Some("\u{1}")` になる。iced 0.14.2 の既定はこれを見て `None` を返し、
    /// せっかく `Home` へ変換した結果を捨てる（ADR-0016）。**その入力を再現しないと回帰にならない。**
    fn ctrl_press(c: &str, text: Option<&str>) -> text_editor::KeyPress {
        press(c, keyboard::Modifiers::CTRL, text)
    }

    /// `Ctrl+Shift` の打鍵を組み立てる。`text` は `ctrl_press` と同じ理由（Shift を足しても
    /// macOS が返すのは Ctrl 由来の制御文字のまま）で制御文字を使う。
    fn ctrl_shift_press(c: &str, text: Option<&str>) -> text_editor::KeyPress {
        press(c, keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT, text)
    }

    /// **macOS の Control 系編集操作が本文で効くこと。**
    ///
    /// `text` が制御文字のとき（macOS の実機）と `None` のとき（他の経路）の両方で固定する。
    /// 片方だけ通しても、実機で効かない実装が通ってしまう。
    #[test]
    fn macos_control_keys_move_the_cursor_in_the_body() {
        use iced::advanced::text::editor::Motion;

        let expected = [
            ("a", '\u{1}', Motion::Home),
            ("e", '\u{5}', Motion::End),
            ("b", '\u{2}', Motion::Left),
            ("f", '\u{6}', Motion::Right),
            ("n", '\u{e}', Motion::Down),
            ("p", '\u{10}', Motion::Up),
        ];

        for (c, control, motion) in expected {
            for text in [Some(control.to_string()), None] {
                let binding = editor_key_binding(ctrl_press(c, text.as_deref()));
                assert!(
                    matches!(binding, Some(text_editor::Binding::Move(m)) if m == motion),
                    "Ctrl+{c}（text: {text:?}）が {motion:?} にならない: {binding:?}",
                );
            }
        }
    }

    /// **`Ctrl+D` は後ろを 1 文字、`Ctrl+H` は前を 1 文字消すこと。**
    ///
    /// `Ctrl+H` は iced の既定が処理できているので自前では変換していない。
    /// **どちらの層が担っているかではなく、効くことを固定する**（既定が変わったら気づける）。
    #[test]
    fn macos_control_keys_delete_one_character() {
        assert!(matches!(
            editor_key_binding(ctrl_press("d", Some("\u{4}"))),
            Some(text_editor::Binding::Delete),
        ));
        assert!(matches!(
            editor_key_binding(ctrl_press("h", Some("\u{8}"))),
            Some(text_editor::Binding::Backspace),
        ));
    }

    /// 打鍵を 1 文字ずつ流す（`Edit::Insert` の連続 = 実際の入力と同じ経路）。
    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            send(app, insert(c));
        }
    }

    /// **`Cmd+Z` で直前の編集が戻り、カーソルもその位置へ戻ること。**
    #[test]
    fn undo_restores_the_previous_text_and_cursor() {
        let (_dir, mut app) = app_with_vault("undo-basic");
        send(&mut app, Message::NoteSelected(0));
        replace_content(&mut app, "元の本文");
        let before = app.content.cursor();

        type_text(&mut app, "XY");
        assert_eq!(app.content.text(), "XY元の本文");

        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        assert_eq!(app.content.text(), "元の本文", "元に戻っていない");
        assert_eq!(app.content.cursor(), before, "カーソルが戻っていない");
    }

    /// **連続した同じ種類の編集は 1 ステップ、種類が変わったら区切ること**（ADR-0017）。
    ///
    /// 1 打鍵 1 ステップだと「あいう」を消すのに 3 回押すことになる。逆に全部を 1 つに
    /// まとめると、消したつもりの無いところまで巻き戻る。**その境目をここで固定する。**
    #[test]
    fn undo_groups_the_same_kind_of_edit_and_breaks_on_a_different_one() {
        use iced::advanced::text::editor::{Action, Edit};

        let (_dir, mut app) = app_with_vault("undo-grouping");
        send(&mut app, Message::NoteSelected(0));
        replace_content(&mut app, "");

        type_text(&mut app, "今日の予定"); // 入力（1 ステップ）
        send(&mut app, Message::Edit(Action::Edit(Edit::Backspace)));
        send(&mut app, Message::Edit(Action::Edit(Edit::Backspace))); // 削除（1 ステップ）
        type_text(&mut app, "表"); // また入力（1 ステップ）
        assert_eq!(app.content.text(), "今日の表");

        let undo = |app: &mut App| send(app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        undo(&mut app);
        assert_eq!(app.content.text(), "今日の", "最後の入力だけが戻っていない");
        undo(&mut app);
        assert_eq!(app.content.text(), "今日の予定", "削除がまとめて戻っていない");
        undo(&mut app);
        assert_eq!(app.content.text(), "", "入力がまとめて戻っていない");
    }

    /// **カーソルを動かしたら、そこで区切ること。**
    ///
    /// 切らないと、行頭で打った文字と別の場所で打った文字が 1 回の undo でまとめて消える。
    #[test]
    fn moving_the_cursor_starts_a_new_undo_step() {
        use iced::advanced::text::editor::{Action, Motion};

        let (_dir, mut app) = app_with_vault("undo-cursor-break");
        send(&mut app, Message::NoteSelected(0));
        replace_content(&mut app, "");

        type_text(&mut app, "あ");
        send(&mut app, Message::Edit(Action::Move(Motion::Home)));
        type_text(&mut app, "い");
        assert_eq!(app.content.text(), "いあ");

        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        assert_eq!(app.content.text(), "あ", "移動をまたいで 1 ステップにまとめている");
    }

    /// **`Cmd+Shift+Z` でやり直せること。やり直したあとに編集したら、その先は捨てること。**
    #[test]
    fn redo_puts_the_edit_back_and_a_new_edit_drops_the_rest() {
        let (_dir, mut app) = app_with_vault("undo-redo");
        send(&mut app, Message::NoteSelected(0));
        replace_content(&mut app, "");
        type_text(&mut app, "あ");

        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));
        assert_eq!(app.content.text(), "");

        send(
            &mut app,
            pressed(
                key("z"),
                keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT,
            ),
        );
        assert_eq!(app.content.text(), "あ", "やり直せていない");

        // 戻してから別の編集をしたら、やり直せるはずだった歴史はもう繋がらない。
        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));
        type_text(&mut app, "い");
        assert!(app.redo.is_empty(), "新しい編集で redo が捨てられていない");
    }

    /// **ノートを切り替えたら履歴を捨てること。これは事故防止の核心。**
    ///
    /// 残っていると、切り替えた先で `Cmd+Z` を押した瞬間に**開いていないノートの本文**が
    /// 今のノートへ書き込まれ、自動保存がそれをディスクまで届ける。
    #[test]
    fn switching_notes_drops_the_undo_history() {
        let (_dir, mut app) = app_with_vault("undo-switch");
        send(&mut app, Message::NoteSelected(0));
        type_text(&mut app, "X");
        // 先に保存を済ませる。dirty のまま切り替えると**保存の完了まで切替が待たされる**
        // （ADR-0014）ので、切り替わっていない状態でこのテストが通ってしまう。
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);

        send(&mut app, Message::NoteSelected(1));
        assert_eq!(app.selected, Some(1), "前提: 切替が起きていない");
        let after_switch = app.content.text();
        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        assert!(app.undo.is_empty(), "切替で履歴が残っている");
        assert_eq!(
            app.content.text(),
            after_switch,
            "別のノートの本文が今のノートへ入った"
        );
    }

    /// **パレットを開いている間の `Cmd+Z` は本文に効かないこと。**
    ///
    /// 検索欄へ打っているときの ⌘Z が「見ていない本文を書き換える」操作になってはいけない
    /// （Control 系をフォーカスで塞いだのと同じ理由。ADR-0016）。
    #[test]
    fn undo_does_not_fire_while_the_palette_is_open() {
        let (_dir, mut app) = app_with_vault("undo-palette");
        send(&mut app, Message::NoteSelected(0));
        replace_content(&mut app, "");
        type_text(&mut app, "あ");

        open_palette(&mut app, "");
        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        assert_eq!(app.content.text(), "あ", "パレット表示中に本文が戻った");
    }

    /// **戻した本文もディスクへ書くこと。** 画面とファイルが食い違ったままになると、
    /// 次に開いたときに戻したはずの編集が復活する。
    #[test]
    fn undo_marks_the_note_for_saving() {
        let (_dir, mut app) = app_with_vault("undo-saves");
        send(&mut app, Message::NoteSelected(0));
        let path = app.notes[0].path.clone();
        type_text(&mut app, "X");
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);
        // **前提の確認。** ここで書けていないと、このテストは何も検証しないまま緑になる。
        assert!(
            std::fs::read_to_string(&path).unwrap().contains('X'),
            "前提: 編集がディスクへ届いていない"
        );

        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));
        assert!(is_dirty(&app), "戻したのに保存対象になっていない");
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));
        flush_saves(&mut app);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains('X'), "戻した編集がディスクに残っている");
    }

    /// `Ctrl+K` は `update()` へ回すこと（`Binding::Sequence` では組めない。ADR-0016）。
    #[test]
    fn ctrl_k_is_handled_by_the_update_loop() {
        assert!(matches!(
            editor_key_binding(ctrl_press("k", Some("\u{b}"))),
            Some(text_editor::Binding::Custom(Message::CutToLineEnd)),
        ));
    }

    /// **`Ctrl+K` がカーソルから行末までを実際に消すこと。**
    ///
    /// **binding の形だけを見ていたせいで、選択されるだけで何も消えない実装を通した。**
    /// `Binding::Sequence(vec![Select(End), Cut])` は `Cut` が選択前の `Content` を読むので
    /// 永遠に空振りする（ADR-0016）。**効果を本文の文字列で見る**テストでしか捕まらない。
    #[test]
    fn ctrl_k_cuts_from_the_cursor_to_the_end_of_the_line() {
        use iced::advanced::text::editor::{Action, Motion};

        let (_dir, mut app) = app_with_vault("ctrl-k-cut");
        send(&mut app, Message::NoteSelected(0));
        app.content = text_editor::Content::with_text("一行目\n二行目");
        send(&mut app, Message::Edit(Action::Move(Motion::Right)));

        send(&mut app, Message::CutToLineEnd);

        assert_eq!(app.content.text(), "一\n二行目", "行末まで消えていない");
        assert!(is_dirty(&app), "編集したのに dirty が立っていない");
    }

    /// **行末・空行の `Ctrl+K` では何も起きないこと**（`Backspace` で組むと手前が消える）。
    ///
    /// 本文が変わらないだけでなく **dirty も立たない**ことまで見る。立ててしまうと、
    /// 何も編集していないのに自動保存が走って mtime が動く（`docs/spec.md` の
    /// 「無編集の切替で mtime が動かない」と同じ約束）。
    #[test]
    fn ctrl_k_does_nothing_at_the_end_of_a_line() {
        use iced::advanced::text::editor::{Action, Motion};

        let (_dir, mut app) = app_with_vault("ctrl-k-line-end");
        send(&mut app, Message::NoteSelected(0));

        for (text, where_) in [("一行目\n二行目", "行末"), ("\n二行目", "空行")] {
            app.content = text_editor::Content::with_text(text);
            app.content.perform(Action::Move(Motion::End));
            app.dirty_since = None;

            send(&mut app, Message::CutToLineEnd);

            assert_eq!(app.content.text(), text, "{where_} で本文が変わった");
            assert!(!is_dirty(&app), "{where_} で dirty が立った（無編集で保存が走る）");
        }
    }

    /// **フォーカスが無いエディタでは Control 系を効かせないこと。**
    ///
    /// `key_binding` はフォーカスの有無に関わらず呼ばれる。パレットを開いている間に
    /// `Ctrl+N` を押すと一覧が下へ動くが、**同じ打鍵で裏の本文のカーソルまで動いてはいけない**。
    /// `Ctrl+D` に至っては、見ていない本文から 1 文字消える。
    #[test]
    fn control_keys_do_nothing_while_the_editor_is_unfocused() {
        for c in ["a", "e", "n", "p", "d", "k"] {
            let mut kp = ctrl_press(c, Some("\u{1}"));
            kp.status = text_editor::Status::Active;
            assert!(
                editor_key_binding(kp).is_none(),
                "フォーカスの無いエディタが Ctrl+{c} に反応している",
            );
        }
    }

    /// **`Ctrl` に他の修飾が乗った組み合わせは拾わないこと。**
    ///
    /// `Ctrl+Shift+A`（選択のつもり）や `Ctrl+Alt+A` を行頭移動にしない。
    /// `Cmd+Ctrl+A` はここには含めない — 既定が `SelectAll` を返す組み合わせで、
    /// **既定が答えを出しているものは触らない**のがこの関数の約束（ADR-0016）。
    #[test]
    fn control_bindings_require_control_alone() {
        // `Shift` は ADR-0023 で選択の拡張として意味を持たせたので、ここでは弾かない
        // （下の `macos_control_shift_selects_in_the_body` が Shift 側を固定する）。
        let mut kp = ctrl_press("a", Some("\u{1}"));
        kp.modifiers = keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT;
        assert!(
            editor_key_binding(kp).is_none(),
            "Ctrl+Alt+A まで拾っている",
        );
    }

    /// **`Ctrl+Shift+<移動系>` は選択を拡張すること（ADR-0023）。**
    ///
    /// ADR-0016 は `Ctrl+Shift+A` を意図的に弾いていたが、それは「未定義のまま拾わない」
    /// という用心であって「選択には使わない」という決定ではなかった。iced 既定の矢印キーが
    /// Shift で `Move` と `Select` を切り替えるのと同じ形に揃える。
    #[test]
    fn macos_control_shift_selects_in_the_body() {
        use iced::advanced::text::editor::Motion;

        let expected = [
            ("a", '\u{1}', Motion::Home),
            ("e", '\u{5}', Motion::End),
            ("b", '\u{2}', Motion::Left),
            ("f", '\u{6}', Motion::Right),
            ("n", '\u{e}', Motion::Down),
            ("p", '\u{10}', Motion::Up),
        ];

        for (c, control, motion) in expected {
            let binding = editor_key_binding(ctrl_shift_press(c, Some(&control.to_string())));
            assert!(
                matches!(binding, Some(text_editor::Binding::Select(m)) if m == motion),
                "Ctrl+Shift+{c} が {motion:?} の選択にならない: {binding:?}",
            );
        }
    }

    /// **`Ctrl+Shift+D` / `Ctrl+Shift+K` は何もしないこと。**
    ///
    /// 削除・切り取り系は shift 版の意味を定義していない（ADR-0023）。ここを拾うと、
    /// 選択したつもりで `Ctrl+D` / `Ctrl+K` と同じ削除が走り、本文が消える事故になる。
    #[test]
    fn macos_control_shift_delete_keys_stay_undefined() {
        for (c, control) in [("d", '\u{4}'), ("k", '\u{b}')] {
            let binding = editor_key_binding(ctrl_shift_press(c, Some(&control.to_string())));
            assert!(binding.is_none(), "Ctrl+Shift+{c} が拾われている: {binding:?}");
        }
    }

    /// 修飾なしの打鍵はそのまま入ること（塞ぎすぎていないことの裏取り）。
    #[test]
    fn plain_keys_still_insert() {
        assert!(matches!(
            editor_key_binding(key_press("p", <_>::default())),
            Some(text_editor::Binding::Insert('p')),
        ));
    }

    /// **`Cmd+C` が死んでいないこと。** `command()` で一律に落とす実装だと、
    /// 混入は止まる代わりにコピー・貼り付け・全選択がまとめて使えなくなる。
    #[test]
    fn command_shortcuts_of_the_editor_itself_survive() {
        assert!(matches!(
            editor_key_binding(key_press("c", keyboard::Modifiers::COMMAND)),
            Some(text_editor::Binding::Copy),
        ));
        assert!(matches!(
            editor_key_binding(key_press("v", keyboard::Modifiers::COMMAND)),
            Some(text_editor::Binding::Paste),
        ));
        assert!(matches!(
            editor_key_binding(key_press("a", keyboard::Modifiers::COMMAND)),
            Some(text_editor::Binding::SelectAll),
        ));
    }

    /// **⌘ 押下中の打鍵がパレットの検索欄に入らないこと。**
    ///
    /// パレットを開いたまま `Cmd+P` を押し直したときの経路。`text_input` には
    /// `key_binding` が無いので、混入はここ（メッセージを受け取る側）でしか止められない。
    #[test]
    fn command_shortcuts_do_not_reach_the_palette_query() {
        let (_dir, mut app) = app_with_folders("cmd-palette", &[("topics", "あ")]);
        open_palette(&mut app, "graphql");

        app.modifiers = keyboard::Modifiers::COMMAND;
        let _ = update(&mut app, Message::PaletteQueryChanged("graphqlp".to_string()));
        assert_eq!(app.palette.as_ref().unwrap().query, "graphql", "検索欄に p が入った");

        // ⌘ を離せば普通に打てる（塞ぎすぎていないこと）。
        app.modifiers = keyboard::Modifiers::empty();
        let _ = update(&mut app, Message::PaletteQueryChanged("graphqls".to_string()));
        assert_eq!(app.palette.as_ref().unwrap().query, "graphqls");
    }

    /// **⌘ 押下中の打鍵がリネーム欄に入らないこと。**
    ///
    /// ここは Enter で**ファイル名としてディスクに届く**ので、混入が実害になる。
    #[test]
    fn command_shortcuts_do_not_reach_the_rename_input() {
        let (_dir, mut app) = app_with_folders("cmd-rename", &[("topics", "設計メモ")]);
        app.rename = Some("設計メモ.md".to_string());

        app.modifiers = keyboard::Modifiers::COMMAND;
        let _ = update(&mut app, Message::RenameChanged("設計メモ.mdn".to_string()));
        assert_eq!(app.rename.as_deref(), Some("設計メモ.md"), "ファイル名に n が入った");

        app.modifiers = keyboard::Modifiers::empty();
        let _ = update(&mut app, Message::RenameChanged("新しい名前.md".to_string()));
        assert_eq!(app.rename.as_deref(), Some("新しい名前.md"));
    }

    /// **⌘ の押しっぱなしが、フォーカスを失った時点で解けること。**
    ///
    /// `ModifiersChanged` はフォーカスを持っている間しか来ない。`Cmd+Tab` で抜けて
    /// 向こうで ⌘ を離すと通知が来ず、戻ってきたときに入力欄が黙って無反応になる。
    /// **安全装置が壊れたときに何が起きるか**まで含めての対処（ADR-0010）。
    #[test]
    fn losing_focus_releases_a_stuck_command_key() {
        let (_dir, mut app) = app_with_folders("stuck-cmd", &[("topics", "あ")]);
        app.rename = Some("あ.md".to_string());
        app.modifiers = keyboard::Modifiers::COMMAND;

        let _ = update(&mut app, Message::WindowUnfocused);
        assert!(app.modifiers.is_empty(), "⌘ が押しっぱなしのまま残っている");

        // 解けているので、戻ってきたあとは普通に打てる。
        let _ = update(&mut app, Message::RenameChanged("い.md".to_string()));
        assert_eq!(app.rename.as_deref(), Some("い.md"), "戻ってきても打てない");
    }

    /// **エディタに入れて出しただけの本文が、元ファイルと 1 バイトも違わないこと。**
    ///
    /// ここがずれると `save_now()` の「内容が変わっていなければ書かない」判定が常に外れ、
    /// **無編集でノートを開いただけで毎回ディスクに書く**。mtime が動き続けて外部ツールから
    /// 更新と誤認される（Done の定義に明記された禁止事項）。
    #[test]
    fn editor_roundtrip_is_byte_identical() {
        let raw = "---\ntitle: \"テスト\"\ntags: [a, b]\n---\n\n# 見出し\n\n\
                   日本語の本文。漢字が消えないこと。\n\n\
                   ```rust\nfn main() {}\n```\n";
        // 型注釈は必須。`()` も `text::Renderer` を実装しているので、注釈が無いと
        // 推論が決まらない（そして `()` は何もしないダミーなので、この検証には使えない）。
        let content: text_editor::Content = text_editor::Content::with_text(raw);
        assert_eq!(content.text(), raw, "エディタを往復しただけで本文が変わっている");
    }

    /// **空のノートでも往復が一致すること。** `Cmd+N` が作るのは空ファイルなので、
    /// ここで改行が 1 つ生えると、作った直後に開いて閉じるだけで書き込みが走る。
    #[test]
    fn editor_roundtrip_keeps_an_empty_note_empty() {
        let content: text_editor::Content = text_editor::Content::with_text("");
        assert_eq!(content.text(), "");
    }

    /// 末尾改行が無いファイルでも往復が一致すること。
    ///
    /// 実 vault（約 1200 件）は全ファイルが改行で終わっていたので今日は踏まないが、
    /// 他ツールや将来の `Cmd+N` が改行なしのファイルを作った瞬間に
    /// 「開くたびに書く」へ化ける。境界はデータの現状ではなくコードで押さえる。
    #[test]
    fn editor_roundtrip_keeps_missing_trailing_newline() {
        let raw = "# 見出し\n\n改行で終わらない本文。";
        let content: text_editor::Content = text_editor::Content::with_text(raw);
        assert_eq!(content.text(), raw);
    }

    /// `dirty_since` を打鍵のたびに更新してしまうと、上限が永遠に来ないことの明示。
    /// **この関数の正しさは呼び出し側の記録の仕方に依存する**ので、退行の目印として残す。
    #[test]
    fn max_wait_never_fires_if_dirty_since_is_reset_on_every_keystroke() {
        let start = Instant::now();
        // 10 秒打ち続けたが、dirty_since を毎回 now に更新してしまった世界。
        let now = start + Duration::from_secs(10);
        let last_edit = now - Duration::from_millis(50);
        let dirty_since_wrongly_reset = last_edit;
        assert!(!should_save(now, last_edit, dirty_since_wrongly_reset, None));
    }

    // ── 初回セットアップ（ADR-0015）─────────────────────────────
    //
    // 固定したいのは**起動経路の優先順位**と、**保存先が決まる前に何も書かないこと**。
    // 「ノートが消えたように見える」経路を作らないための安全装置がここに集まっている。

    /// 使い捨ての作業ディレクトリ。`$HOME` の代わりに使う。
    /// セットアップ画面から始まる `App`。**設定ファイルを使い捨てのパスへ向ける**
    /// （本物の `~/Library/Application Support` を汚さない）。
    fn app_in_setup(home: &Path, suggested: PathBuf) -> App {
        let mut app = boot_setup(suggested);
        app.config_file = config::config_file(home);
        app
    }

    #[test]
    fn vault_env_wins_over_the_remembered_path() {
        let home = TempDir::new("env-wins");
        let env_root = home.join("env-vault");
        let remembered = home.join("remembered-vault");
        std::fs::create_dir_all(&env_root).unwrap();
        std::fs::create_dir_all(&remembered).unwrap();

        let choice = resolve_vault(
            Some(env_root.display().to_string()),
            Some(remembered),
            config::default_vault(&home),
        );

        assert_eq!(choice, VaultChoice::Ready(env_root));
    }

    /// **明示的に指定されたものが外れているなら、代わりを探さない**（ADR-0002 / ADR-0015）。
    /// ここでセットアップへ倒すと、借り物 vault を指したつもりで別の場所へ書き始める。
    #[test]
    fn an_invalid_vault_env_refuses_to_start() {
        let home = TempDir::new("env-invalid");
        let missing = home.join("not-there");

        let choice = resolve_vault(
            Some(missing.display().to_string()),
            // 記憶があっても、そちらへ倒さないことが要点。
            Some(home.to_path_buf()),
            config::default_vault(&home),
        );

        assert!(matches!(choice, VaultChoice::Refuse(_)), "{choice:?}");
    }

    #[test]
    fn the_remembered_path_opens_without_setup() {
        let home = TempDir::new("remembered");
        let remembered = home.join("Documents/somewhere else");
        std::fs::create_dir_all(&remembered).unwrap();

        let choice = resolve_vault(None, Some(remembered.clone()), config::default_vault(&home));

        assert_eq!(choice, VaultChoice::Ready(remembered));
    }

    /// 記憶が指す先が消えていたら**セットアップへ戻す**。既定パスへ黙って倒すと、
    /// 同期の失敗などで vault が見えないときに「空になった」と誤解する。
    #[test]
    fn a_remembered_path_that_vanished_falls_back_to_setup() {
        let home = TempDir::new("vanished");
        let suggested = config::default_vault(&home);

        let choice = resolve_vault(None, Some(home.join("gone")), suggested.clone());

        assert_eq!(choice, VaultChoice::NeedsSetup { suggested });
    }

    #[test]
    fn no_memory_at_all_starts_setup() {
        let home = TempDir::new("first-run");
        let suggested = config::default_vault(&home);

        let choice = resolve_vault(None, None, suggested.clone());

        assert_eq!(choice, VaultChoice::NeedsSetup { suggested });
    }

    /// **保存先が決まる前に 1 バイトも書かない。** 打鍵と tick を通してしまうと、
    /// まだ候補でしかないパスへ自動保存が走る。
    #[test]
    fn nothing_reaches_the_disk_while_the_setup_screen_is_up() {
        let home = TempDir::new("no-writes");
        let suggested = config::default_vault(&home);
        let mut app = app_in_setup(&home, suggested.clone());
        let before = app.content.text();

        send(&mut app, insert('あ'));
        send(&mut app, Message::Tick(Instant::now() + Duration::from_secs(5)));
        send(&mut app, Message::NewNote);

        assert!(app.setup.is_some(), "セットアップ画面から出てしまった");
        assert_eq!(app.content.text(), before, "エディタが編集を受け付けた");
        assert_eq!(app.saves, 0);
        assert!(!suggested.exists(), "候補のフォルダが作られている");
        assert!(!app.config_file.exists(), "選ぶ前に記憶が書かれている");
    }

    #[test]
    fn adopting_the_suggested_folder_creates_remembers_and_loads_it() {
        let home = TempDir::new("adopt");
        let suggested = config::default_vault(&home);
        // 既にメモが入っているフォルダを選んだ場合も、そのまま開けること。
        std::fs::create_dir_all(&suggested).unwrap();
        std::fs::write(suggested.join("既存.md"), "# 既存のメモ\n").unwrap();
        let mut app = app_in_setup(&home, suggested.clone());

        send(&mut app, Message::SetupUseSuggested);

        assert!(app.setup.is_none(), "セットアップが閉じていない");
        assert_eq!(app.root, suggested);
        assert_eq!(app.notes.len(), 1);
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.error, None);
        assert_eq!(config::read_vault(&app.config_file), Some(suggested));
    }

    /// フォルダが無い場合は作って始める（Boostnote と同じ。ADR-0015）。
    #[test]
    fn adopting_a_folder_that_does_not_exist_yet_creates_it() {
        let home = TempDir::new("adopt-new");
        let suggested = config::default_vault(&home);
        let mut app = app_in_setup(&home, suggested.clone());

        send(&mut app, Message::SetupUseSuggested);

        assert!(suggested.is_dir(), "保存先が作られていない");
        assert!(app.setup.is_none());
        assert!(app.notes.is_empty());
    }

    /// **失敗したらセットアップ画面に留まる。** 空の vault を開いて先へ進むと、
    /// そこへ書いたものが「消えた」ように見える（ADR-0002 と同じ穴）。
    #[test]
    fn a_folder_that_cannot_be_created_keeps_the_setup_screen_with_a_reason() {
        let home = TempDir::new("adopt-fails");
        // ファイルの下にディレクトリは作れない。許可を拒否されたときと同じ形の失敗。
        let blocker = home.join("これはファイル");
        std::fs::write(&blocker, "").unwrap();
        let doomed = blocker.join("haboku");
        let mut app = app_in_setup(&home, doomed.clone());

        send(&mut app, Message::SetupUseSuggested);

        let setup = app.setup.as_ref().expect("セットアップ画面から出てしまった");
        let reason = setup.error.as_ref().expect("理由が表示されていない");
        assert!(reason.contains("保存先を用意できませんでした"), "{reason}");
        assert!(app.notes.is_empty());
    }

    /// 記憶に失敗しても**開くのは続ける**（書いたものは失われない）。ただし黙らない。
    #[test]
    fn a_vault_that_cannot_be_remembered_still_opens_but_says_so() {
        let home = TempDir::new("cannot-remember");
        let suggested = config::default_vault(&home);
        let mut app = app_in_setup(&home, suggested.clone());
        // 設定ファイルの親を作れない場所へ向ける。
        let blocker = home.join("これもファイル");
        std::fs::write(&blocker, "").unwrap();
        app.config_file = blocker.join("haboku/vault");

        send(&mut app, Message::SetupUseSuggested);

        assert!(app.setup.is_none(), "開けていない");
        assert_eq!(app.root, suggested);
        let error = app.error.as_ref().expect("黙って失敗している");
        assert!(error.contains("覚えられませんでした"), "{error}");
    }

    /// 取り消し（フォルダ選択を閉じた）で**勝手に候補を採用しない**。
    #[test]
    fn cancelling_the_folder_dialog_changes_nothing() {
        let home = TempDir::new("cancel");
        let suggested = config::default_vault(&home);
        let mut app = app_in_setup(&home, suggested.clone());

        send(&mut app, Message::SetupPicked(None));

        assert!(app.setup.is_some());
        assert!(!suggested.exists());
    }

    /// **セットアップ中でも窓は閉じられること。** `exit_on_close_request` を false に
    /// してあるので、ここを落とすと ✕ も `Cmd+Q` も効かない窓になる。
    #[test]
    fn the_window_can_still_be_closed_during_setup() {
        let home = TempDir::new("close");
        let mut app = app_in_setup(&home, config::default_vault(&home));

        // `Task` の中身は iced ランタイムのものなので、ここでは「空の Task を返して
        // 黙殺していない」ことだけ見る。
        let task = update(&mut app, Message::CloseRequested(iced::window::Id::unique()));

        assert_ne!(task.units(), 0, "閉じる要求が無視されている");
    }

    // ── プレビュー（ADR-0019）───────────────────────────────

    /// `Cmd+Shift+V` でプレビューに入り、再押下で編集へ戻る。Escape でも戻る。
    ///
    /// ADR-0004 のトグル不成立は「フォーカス中の入力欄への混入」が理由だった。
    /// プレビュー中はエディタが view に無く漏れる先が無いので、ここはトグルでよい。
    #[test]
    fn preview_toggles_with_cmd_shift_v_and_escape() {
        let (_dir, mut app) = app_with_vault("preview-toggle");
        send(&mut app, Message::NoteSelected(0));
        let cmd_shift = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;

        send(&mut app, pressed(key("v"), cmd_shift));
        assert!(app.preview.is_some(), "Cmd+Shift+V でプレビューに入っていない");

        send(&mut app, pressed(key("v"), cmd_shift));
        assert!(app.preview.is_none(), "再押下で編集へ戻っていない");

        send(&mut app, pressed(key("v"), cmd_shift));
        send(&mut app, pressed(named(keyboard::key::Named::Escape), keyboard::Modifiers::empty()));
        assert!(app.preview.is_none(), "Escape で編集へ戻っていない");
    }

    /// **プレビューの出入りは編集ではないこと。** dirty が立つと、無編集のノート切替で
    /// ディスクに書く（mtime が動く）ようになり、Done の定義に反する。
    #[test]
    fn toggling_preview_does_not_mark_dirty() {
        let (_dir, mut app) = app_with_vault("preview-clean");
        send(&mut app, Message::NoteSelected(0));
        let cmd_shift = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;

        send(&mut app, pressed(key("v"), cmd_shift));
        send(&mut app, pressed(key("v"), cmd_shift));

        assert!(!is_dirty(&app), "表示を切り替えただけで dirty が立った");
        assert_eq!(app.saves, 0, "表示を切り替えただけでディスクに書いた");
    }

    /// **プレビュー中にノートを切り替えたら、プレビューのまま新しい本文になること。**
    ///
    /// 表示だけ古いままだと「開いたのに前のノートが見えている」となり、読み歩きが成立しない。
    #[test]
    fn preview_follows_note_switch() {
        let dir = TempDir::new("preview-switch");
        // 段落数を変えて、パース結果の項目数で「どちらの本文か」を見分けられるようにする。
        std::fs::write(dir.join("a.md"), "# 一段落\n").unwrap();
        std::fs::write(dir.join("b.md"), "# 見出し\n\n本文。\n\n- 箇条書き\n").unwrap();
        let mut app = boot(dir.to_path_buf(), vault::load_dir(&dir), 0.0);

        send(&mut app, Message::NoteSelected(1));
        send(
            &mut app,
            pressed(key("v"), keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT),
        );
        let before = app.preview.as_ref().unwrap().items().len();

        send(&mut app, Message::NoteSelected(0));

        assert_eq!(app.selected, Some(0), "前提: 切替が起きていない");
        let after = app.preview.as_ref().expect("切替でプレビューが畳まれた").items().len();
        assert_ne!(before, after, "プレビューが前のノートの本文のまま");
    }

    /// **プレビュー中の `Cmd+Z` は効かないこと。** 効くと「見えていない本文」を
    /// 書き換え、自動保存がそれをディスクまで届ける（パレット中の ⌘Z と同じ穴）。
    #[test]
    fn undo_is_inert_while_previewing() {
        let (_dir, mut app) = app_with_vault("preview-undo");
        send(&mut app, Message::NoteSelected(0));
        type_text(&mut app, "X");
        let typed = app.content.text();

        send(
            &mut app,
            pressed(key("v"), keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT),
        );
        send(&mut app, pressed(key("z"), keyboard::Modifiers::COMMAND));

        assert_eq!(app.content.text(), typed, "プレビュー中の ⌘Z が本文を書き換えた");
    }

    // ── `Cmd+W` の写像（ADR-0021）────────────────────────────────────────
    //
    // **ここが柱 2 の全部。** `Cmd+W` は既定メニューに Close 項目が無いせいで
    // 届け先を持たず不発だった（実機で確認: 窓ごと生存した）。写像を純関数に
    // 切り出してあるので、閉じる操作の取りこぼしと誤爆を実時間なしで固定できる。

    /// subscription が受け取る形の生イベントを組み立てる。
    fn key_event(k: keyboard::Key, modifiers: keyboard::Modifiers, repeat: bool) -> iced::event::Event {
        let Message::Key(mut event) = pressed(k, modifiers) else {
            unreachable!("pressed は Message::Key を返す");
        };
        if let keyboard::Event::KeyPressed { repeat: r, .. } = &mut event {
            *r = repeat;
        }
        iced::event::Event::Keyboard(event)
    }

    /// `map_event` に流して結果を得る。window Id は写し先の照合に使う。
    fn mapped(event: iced::event::Event, window: iced::window::Id) -> Option<Message> {
        map_event(event, iced::event::Status::Ignored, window)
    }

    /// **`Cmd+W` が閉じる要求になること。** これが写らないと窓が閉じない。
    #[test]
    fn cmd_w_asks_to_close_the_window() {
        let window = iced::window::Id::unique();
        let message = mapped(
            key_event(key("w"), keyboard::Modifiers::COMMAND, false),
            window,
        );

        let Some(Message::CloseRequested(id)) = message else {
            panic!("Cmd+W が閉じる要求にならない: {message:?}");
        };
        assert_eq!(id, window, "別の窓を閉じようとしている");
    }

    /// **オートリピートは無視すること。**
    ///
    /// 保存が失敗している間の 2 段階クローズ（1 回目は拒否、2 回目で `.rescue` 退避）が、
    /// キーの押しっぱなしで**警告を読む間もなく素通りする**のを防ぐ（ADR-0012）。
    #[test]
    fn a_held_down_cmd_w_does_not_close_twice() {
        let message = mapped(
            key_event(key("w"), keyboard::Modifiers::COMMAND, true),
            iced::window::Id::unique(),
        );

        assert!(
            matches!(message, Some(Message::Key(_))),
            "リピートで閉じようとした: {message:?}"
        );
    }

    /// **素の `w` は本文に入ること。** 写像が広すぎると打てない文字ができる。
    #[test]
    fn a_bare_w_is_still_typed() {
        let message = mapped(
            key_event(key("w"), keyboard::Modifiers::empty(), false),
            iced::window::Id::unique(),
        );

        assert!(
            matches!(message, Some(Message::Key(_))),
            "素の w で窓を閉じようとした: {message:?}"
        );
    }

    /// **修飾は完全一致。** `Cmd+Shift+W` では閉じない（ADR-0016 の `Ctrl` と同じ流儀）。
    #[test]
    fn cmd_shift_w_does_not_close_the_window() {
        let modifiers = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;
        let message = mapped(key_event(key("w"), modifiers, false), iced::window::Id::unique());

        assert!(
            matches!(message, Some(Message::Key(_))),
            "Cmd+Shift+W で窓を閉じようとした: {message:?}"
        );
    }

    /// **フォーカス喪失の写像を壊していないこと**（ADR-0010 の ⌘ 解除）。
    #[test]
    fn losing_focus_still_releases_the_command_key() {
        let message = mapped(
            iced::event::Event::Window(iced::window::Event::Unfocused),
            iced::window::Id::unique(),
        );

        assert!(matches!(message, Some(Message::WindowUnfocused)), "{message:?}");
    }
}
