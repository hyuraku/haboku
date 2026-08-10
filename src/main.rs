//! haboku 本体。3 ペイン（フォルダ / 一覧 / エディタ）+ デバウンス自動保存。
//!
//! 実行: `VAULT="$HOME/Documents/haboku" cargo run --release`
//!
//! **必ず release で動かすこと。** debug だと vault 読み込みの数字が一桁変わる
//! （実測: cold 185.1ms / warm 32.8ms @ release・約 1200 件）。

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iced::keyboard;
use iced::widget::{
    button, column, container, mouse_area, rich_text, row, scrollable, span, stack, text,
    text_editor, text_input,
};
use iced::{Color, Element, Fill, Font, Length, Subscription, Task};

use haboku::{fuzzy, vault};

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

/// パレットの入力欄を名指しする ID。開いた瞬間にフォーカスを飛ばすのに要る。
const PALETTE_INPUT_ID: &str = "palette-input";

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

/// サイドバー（フォルダ）の幅。
const SIDEBAR_WIDTH: f32 = 180.0;
/// ノート一覧の幅。
const LIST_WIDTH: f32 = 320.0;

/// 開いているパレットの状態。閉じているときは `None`。
struct Palette {
    query: String,
    /// 絞り込み結果。`(notes のインデックス, マッチ情報)`。
    ///
    /// **`view()` では絞り込みを一切やらない。** クエリが変わった時だけ計算してここに置く。
    /// 打鍵のたびに全ノートを走査する穴（前身の `render()` で開けた穴）を塞ぐため。
    matches: Vec<(usize, fuzzy::Match)>,
    /// いま選んでいる `matches` の位置。
    selected: usize,
}

struct App {
    /// vault のルート。保存後に `parse_note` へ渡すのに要る。
    root: PathBuf,
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

    // ── 自動保存 ────────────────────────────────────────────
    dirty: bool,
    /// dirty になった時刻。上限判定と「未保存」表示に使う。
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
    /// 保存できずに閉じるのを一度断ったか。**2 回目の要求で退避して閉じる**ための記憶。
    ///
    /// 保存が通れば `clear_dirty` で畳む。問題が直ったあとの初回はまた警告から始めたい
    /// （立てっぱなしだと、次に別の失敗を踏んだとき警告なしで終了してしまう）。
    close_refused: bool,
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
}

#[derive(Debug, Clone)]
enum Message {
    /// フォルダを選ぶ。`None` は全件表示に戻す。
    FolderSelected(Option<String>),
    /// ノートを開く（`notes` のインデックス）。
    NoteSelected(usize),
    Edit(text_editor::Action),
    /// 自動保存の監視。dirty の間だけ流れてくる。
    Tick(Instant),
    /// キー入力。フォーカスの位置に関係なく全部流れてくる。
    Key(keyboard::Event),
    PaletteQueryChanged(String),
    /// パレットを閉じる。✕ ボタンと背景クリックから飛ぶ。
    PaletteClose,
    /// ノートを新規作成する（`Cmd+N`）。
    NewNote,
    /// リネームを開始する（`Cmd+R`）。入力欄に現在のファイル名を入れて開く。
    RenameStarted,
    RenameChanged(String),
    RenameCancel,
    RenameCommit,
    /// 開いているノートを `.trash` へ退避する（`Cmd+Delete`）。
    DeleteNote,
    /// ウィンドウを閉じる要求（`Cmd+W`・✕・`Cmd+Q`）。
    ///
    /// **既定の `exit_on_close_request = true` のままだと、これを受け取る前にプロセスが
    /// 終わる。** デバウンス（1 秒）の途中で閉じれば、その分の編集はディスクにも
    /// メモリにも残らず消える。`main` で false にして、保存を挟めるようにしてある。
    CloseRequested(iced::window::Id),
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
fn should_save(now: Instant, last_edit: Instant, dirty_since: Instant) -> bool {
    now.duration_since(last_edit) >= AUTOSAVE_DEBOUNCE
        || now.duration_since(dirty_since) >= AUTOSAVE_MAX_WAIT
}

/// 等幅フォント。**`Font::MONOSPACE` は使わない**（漢字が消える。ADR-0003）。
///
/// 「等幅なら何でもいい」を意味する `Font::MONOSPACE` を渡すと、cosmic-text が漢字に
/// macOS の `GB18030 Bitmap` を選び、Swash がラスタライズに失敗してグリフごと捨てる。
/// 等幅は必ず**名指し**する。実データ 約 1200 件で漢字の欠落が無く、コードブロックの桁も
/// 揃うことを目視で確認済み。
const EDITOR_FONT: Font = Font::with_name("Osaka-Mono");

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

/// 一覧に出すノートの index を作り直す。フォルダ選択が変わった時だけ呼ぶ。
fn visible_indices(notes: &[vault::Note], folder: Option<&str>) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, note)| folder.is_none_or(|f| note.folder == f))
        .map(|(i, _)| i)
        .collect()
}

fn boot(root: PathBuf, notes: Vec<vault::Note>, load_ms: f64) -> App {
    let folders = count_folders(&notes);
    let visible = visible_indices(&notes, None);

    App {
        root,
        notes,
        folders,
        selected_folder: None,
        visible,
        selected: None,
        content: text_editor::Content::with_text(
            "左の一覧からノートを選ぶか、Cmd+P で検索すると、ここに本文が出ます。",
        ),
        palette: None,
        rename: None,
        dirty: false,
        dirty_since: None,
        last_edit: Instant::now(),
        show_marker: false,
        error: None,
        close_refused: false,
        saved_flash_until: None,
        saves: 0,
        load_ms,
        last_view_us: Cell::new(0),
    }
}

/// 保存が済んだ（または差分が無かった）ときに dirty 関連を畳む。
fn clear_dirty(app: &mut App) {
    app.dirty = false;
    app.dirty_since = None;
    app.show_marker = false;
    // 書けたなら、閉じるのを断った記憶も畳む。次に閉じられなくなったときは
    // また警告から始める（`close_refused` の doc 参照）。
    app.close_refused = false;
}

/// 選択中のノートをファイルへ書き戻す。
///
/// **戻り値は「エディタの内容を破棄してよいか」。** false は未保存の内容がディスクに
/// 書けていないことを意味するので、呼び出し元はエディタを上書きする操作（ノート切替）を
/// **中断すること**。進んでしまうと、失われた本文はディスクにもメモリにも残らない。
fn save_now(app: &mut App) -> bool {
    let Some(index) = app.selected else {
        return true;
    };

    let contents = app.content.text();

    // 内容が変わっていないなら書かない。カーソルを動かしただけ・開いただけで mtime が動くと、
    // 外部ツール（git・エディタ・同期）から「更新された」と誤認される。
    if contents == app.notes[index].raw {
        clear_dirty(app);
        return true;
    }

    let path = app.notes[index].path.clone();
    match vault::save(&path, &contents) {
        Ok(()) => {
            // ファイルが正になったので、メモリ側のメタ情報も取り直す。タイトル行を編集したら
            // 一覧にすぐ反映されてほしい。**ここで並べ替えはしない**（`notes` の doc 参照）。
            //
            // mtime は書いた直後の now でよい。次回起動時にディスクから読み直される値であって、
            // ここで 1 回 stat を打ち直すほどの精度は要らない。
            let modified = std::time::SystemTime::now();
            app.notes[index] = vault::parse_note(&app.root, path, contents, modified);
            clear_dirty(app);
            app.error = None;
            app.saves += 1;
            app.saved_flash_until = Some(Instant::now() + SAVED_FLASH);
            true
        }
        Err(e) => {
            // dirty は立てたままにする。表示が消えないことが異常の合図。
            app.error = Some(format!("保存に失敗: {e}"));
            false
        }
    }
}

fn open_note(app: &mut App, index: usize) {
    if let Some(note) = app.notes.get(index) {
        // frontmatter 込みの全文をエディタへ渡す。`.md` が唯一の真実なので、
        // 表示のために本文を加工しない。
        app.content = text_editor::Content::with_text(&note.raw);
        app.selected = Some(index);
    }
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

    // 取り除くと後ろのインデックスが繰り上がる。開いていたのは消したノート自身なので
    // 選択を外し、エディタを空にする（繰り上げの計算そのものを不要にする）。
    app.notes.remove(index);
    app.selected = None;
    app.content = text_editor::Content::with_text("");
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
/// `save_now` が true を返したなら本文は全部ディスクに乗っているので、迷わず閉じてよい。
/// 判断が要るのは **false（書けなかった）** のときで、そこは 2 段階にしてある。
///
/// - **1 回目**: 閉じない。理由をステータス行に出す（気づく機会を作り、手で退避もできる）
/// - **2 回目**: 明確な意思表示とみなし、**本文を `.rescue` へ退避してから**閉じる
///
/// **「閉じない」だけで通さなかった理由。** メモ帳で保存が失敗する現実的な原因は
/// 容量不足・ドライブが外れた・権限が変わった、のどれかで、**待っても直らない**。
/// 踏みとどまり続けても再試行が失敗するだけで、出口は強制終了しかなく、結局本文を失う。
/// それは「安全装置が、失敗したときに何が起きるかまで設計されていない」状態そのもの。
///
/// 逆に 1 回目で退避して閉じると、ユーザーは**何が起きたか知らないまま**終了する。
/// だから警告を 1 回挟む。
fn close_window(app: &mut App, id: iced::window::Id) -> Task<Message> {
    if save_now(app) {
        return iced::window::close(id);
    }

    if !app.close_refused {
        app.close_refused = true;
        app.show_marker = true;
        // **次に何が起きるかまで伝える。** 「閉じられない」だけだと打つ手が分からない。
        if let Some(error) = &mut app.error {
            error.push_str("／もう一度閉じると、本文を .rescue に退避して終了します");
        }
        return Task::none();
    }

    // ── 2 回目。ここから先は必ず閉じる ──
    //
    // 退避先は**優先順で複数**渡す。保存が失敗した原因がノートのあるディレクトリに
    // あるとは限らないし、逆にそこだけの問題なら vault ルートには書ける。
    // 一時ディレクトリは最後の砦（見つけにくいので優先度は最低）。
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

    // 画面はもう無くなるので、伝える経路は stderr しかない。vault 直下に落ちれば
    // Finder からは見えるし、`.rescue` は `load_dir` が拾わないので一覧は汚れない。
    match vault::write_rescue(&dirs, &name, &app.content.text()) {
        Ok(path) => eprintln!("haboku: 保存できなかったので退避しました: {}", path.display()),
        Err(e) => eprintln!("haboku: 退避にも失敗しました（本文は失われます）: {e}"),
    }

    iced::window::close(id)
}

/// クエリで全ノートを絞り込む。**パレットが開いている間ずっとではなく、クエリが変わった時だけ。**
///
/// 絞り込み中のフォルダは無視して**全ノート**を対象にする。`Cmd+P` は「どのフォルダにいても
/// 目的のノートへ飛ぶ」道具なので、いまの絞り込みに引きずられると用を成さない。
fn refilter(notes: &[vault::Note], query: &str) -> Vec<(usize, fuzzy::Match)> {
    let mut hits: Vec<(usize, fuzzy::Match)> = notes
        .iter()
        .enumerate()
        .filter_map(|(i, note)| fuzzy::match_query(query, &note.title).map(|m| (i, m)))
        .collect();

    // スコアの高い順。同点はタイトル順で安定させる（同じクエリで並びが変わらないように）。
    hits.sort_by(|a, b| {
        b.1.score
            .cmp(&a.1.score)
            .then_with(|| notes[a.0].title.cmp(&notes[b.0].title))
    });
    hits.truncate(PALETTE_MAX_RESULTS);
    hits
}

/// パレットが開いている間のキー操作。処理したら `true`。
fn handle_palette_key(app: &mut App, key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    use keyboard::key::Named;

    let Some(palette) = &mut app.palette else {
        return false;
    };
    let len = palette.matches.len();

    match key {
        keyboard::Key::Named(Named::Escape) => {
            app.palette = None;
            true
        }
        keyboard::Key::Named(Named::Enter) => {
            let Some(index) = palette.matches.get(palette.selected).map(|(i, _)| *i) else {
                app.palette = None;
                return true;
            };
            // ここにも保存ガードが要る。パレットからの選択もエディタを上書きする操作。
            // **パレットは閉じない**。閉じてから中断すると、なぜ切り替わらないのかが
            // 分からなくなる（エラーはステータス行に常駐する）。
            if !save_now(app) {
                return true;
            }
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
                app.selected_folder = None;
                app.visible = visible_indices(&app.notes, None);
            }
            open_note(app, index);
            true
        }
        // **矢印キーではなく ctrl-n / ctrl-p。** `text_input` が矢印を消費して親に届かない
        // （gpui 版でも同じ回避策が要った）。
        keyboard::Key::Character(c) if c == "n" && modifiers.control() => {
            if len > 0 {
                palette.selected = (palette.selected + 1) % len;
            }
            true
        }
        keyboard::Key::Character(c) if c == "p" && modifiers.control() => {
            if len > 0 {
                palette.selected = (palette.selected + len - 1) % len;
            }
            true
        }
        _ => false,
    }
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::FolderSelected(folder) => {
            app.visible = visible_indices(&app.notes, folder.as_deref());
            app.selected_folder = folder;
        }
        Message::NoteSelected(index) => {
            // **保存に失敗したら遷移しない。** ここで進むと未保存の本文が
            // ディスクにもメモリにも残らず消える。前身でデータ喪失を招いた欠陥がこれ。
            if !save_now(app) {
                return Task::none();
            }
            open_note(app, index);
        }
        Message::Edit(action) => {
            // カーソル移動やクリックで dirty を立てない。編集だけを拾う。
            let is_edit = matches!(action, iced::advanced::text::editor::Action::Edit(_));
            app.content.perform(action);
            if is_edit && app.selected.is_some() {
                app.dirty = true;
                app.last_edit = Instant::now();
                // **false → true の遷移でだけ**記録する。
                app.dirty_since.get_or_insert_with(Instant::now);
            }
        }
        Message::Tick(now) => {
            if app.saved_flash_until.is_some_and(|until| now >= until) {
                app.saved_flash_until = None;
            }

            if let Some(dirty_since) = app.dirty_since
                && should_save(now, app.last_edit, dirty_since)
            {
                save_now(app);
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
        }
        Message::PaletteQueryChanged(query) => {
            if let Some(palette) = &mut app.palette {
                palette.matches = refilter(&app.notes, &query);
                palette.selected = 0;
                palette.query = query;
            }
        }
        Message::PaletteClose => app.palette = None,
        Message::NewNote => {
            // 作成もエディタを上書きする操作なので、切替と同じ保存ガードを通す。
            if !save_now(app) {
                return Task::none();
            }
            // インデックスがずれるので、古い `matches` を持ったパレットは畳む。
            app.palette = None;
            match create_note(app) {
                Ok(index) => {
                    open_note(app, index);
                    // 作った直後に打ち始められるようにフォーカスを移す。
                    return iced::widget::operation::focus(iced::widget::Id::new(EDITOR_ID));
                }
                Err(e) => app.error = Some(e),
            }
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
            if !save_now(app) {
                return Task::none();
            }
            match commit_rename(app, &input) {
                Ok(()) => {
                    app.rename = None;
                    app.error = None;
                }
                // **入力欄は開いたままにする。** 閉じてしまうと打ち直せない。
                Err(e) => app.error = Some(e),
            }
        }
        Message::DeleteNote => {
            app.palette = None;
            app.rename = None;
            // 捨てる前に書き戻す。`.trash` に残るのが「最後に自動保存された内容」ではなく
            // 「消す直前の内容」になり、誤って消しても打った分まで戻せる。
            // 失敗したら中断（切替・作成・リネームと同じガード）。
            if !save_now(app) {
                return Task::none();
            }
            if let Err(e) = delete_note(app) {
                app.error = Some(e);
            }
        }
        Message::CloseRequested(id) => return close_window(app, id),
        Message::Key(event) => {
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
                app.palette = Some(Palette {
                    query: String::new(),
                    matches: refilter(&app.notes, ""),
                    selected: 0,
                });
                // 開いた瞬間に入力欄へフォーカスを飛ばす。これが無いと「開いたのに打てない」。
                return iced::widget::operation::focus(iced::widget::Id::new(PALETTE_INPUT_ID));
            }

            // `Cmd+N` で新規作成。パレットが開いていても効く（中で畳む）。
            if modifiers.command() && matches!(&key, keyboard::Key::Character(c) if c == "n") {
                return update(app, Message::NewNote);
            }

            // `Cmd+R` でリネーム、`Cmd+Delete` で `.trash` へ退避。
            if modifiers.command() && matches!(&key, keyboard::Key::Character(c) if c == "r") {
                return update(app, Message::RenameStarted);
            }
            if modifiers.command() && matches!(&key, keyboard::Key::Named(keyboard::key::Named::Backspace))
            {
                return update(app, Message::DeleteNote);
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

            handle_palette_key(app, &key, modifiers);
        }
    }
    Task::none()
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
    let keys = iced::event::listen_with(|event, _status, _window| match event {
        iced::event::Event::Keyboard(key_event) => Some(Message::Key(key_event)),
        _ => None,
    });

    // 閉じる要求は **dirty かどうかに関わらず**常に拾う。dirty のときだけ購読すると、
    // 「閉じた瞬間に dirty が解ける」ような競合で取りこぼしたときに黙って終了する。
    let close = iced::window::close_requests().map(Message::CloseRequested);

    if app.dirty || app.saved_flash_until.is_some() {
        Subscription::batch([keys, close, iced::time::every(TICK).map(Message::Tick)])
    } else {
        Subscription::batch([keys, close])
    }
}

fn folder_pane(app: &App) -> Element<'_, Message> {
    // フォルダ名は副文字（霞）。選ばれている行だけ生成りに持ち上げ、件数は常に霞のまま。
    let all_selected = app.selected_folder.is_none();
    let all = button(
        row![
            text("すべて")
                .size(12)
                .color(if all_selected { KINARI } else { KASUMI })
                .width(Fill),
            text(app.notes.len().to_string()).size(12).color(KASUMI),
        ]
        .padding(2),
    )
    .on_press(Message::FolderSelected(None))
    .width(Fill)
    .style(if all_selected { selected_row } else { button::text });

    let items = app.folders.iter().map(|(name, count)| {
        let selected = app.selected_folder.as_deref() == Some(name.as_str());
        button(
            row![
                text(name)
                    .size(12)
                    .color(if selected { KINARI } else { KASUMI })
                    .width(Fill),
                text(count.to_string()).size(12).color(KASUMI),
            ]
            .padding(2),
        )
        .on_press(Message::FolderSelected(Some(name.clone())))
        .width(Fill)
        .style(if selected { selected_row } else { button::text })
        .into()
    });

    scrollable(column(std::iter::once(all.into()).chain(items)).spacing(1))
        .height(Fill)
        .into()
}

fn note_pane(app: &App) -> Element<'_, Message> {
    // 全件（約 1200 件）をそのまま並べる。仮想リストは要らないと spike で実測済み
    // （打鍵時の `view()` 構築 0.13ms）。無い機能を先回りで作らない。
    let rows = app.visible.iter().map(|&index| {
        let note = &app.notes[index];
        let selected = app.selected == Some(index);
        // タイトルは生成り、選択中だけ琥珀の明るい側へ。プレビューは常に霞。
        let body = column![
            text(&note.title)
                .size(13)
                .color(if selected { KOHAKU_SOFT } else { KINARI }),
            text(&note.preview).size(10).color(KASUMI),
        ]
        .spacing(2);

        button(body)
            .on_press(Message::NoteSelected(index))
            .width(Fill)
            .style(if selected { selected_row } else { button::text })
            .into()
    });

    scrollable(column(rows).spacing(1)).height(Fill).into()
}

fn editor_pane(app: &App) -> Element<'_, Message> {
    text_editor(&app.content)
        .id(iced::widget::Id::new(EDITOR_ID))
        .on_action(Message::Edit)
        .font(EDITOR_FONT)
        // 日本語には単語境界がほぼ無く、既定の `Word` だと長い段落が 1 つの巨大な単語になる。
        .wrapping(iced::advanced::text::Wrapping::WordOrGlyph)
        // syntect の既製テーマではなく自前の Markdown ハイライタ（`highlight.rs` の doc 参照）。
        .highlight_with::<highlight::MarkdownHighlighter>((), markdown_format)
        .style(editor_style)
        .height(Fill)
        .into()
}

/// マッチした文字だけ色を変えたタイトルを作る。
///
/// `fuzzy::Match::ranges` は**バイト範囲**なので `get()` で受ける。日本語タイトルで
/// 文字境界を跨いだときに panic しないため（`&title[range]` だと落ちる）。
fn highlighted_title(title: &str, ranges: &[std::ops::Range<usize>]) -> Element<'static, Message> {
    // 一致文字は琥珀の明るい側 + セミボールド。色だけだと霞んだ地の上で見落とすことがある。
    let hit = KOHAKU_SOFT;
    let hit_font = Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    };
    let mut spans: Vec<iced::advanced::text::Span<'static, ()>> = Vec::new();
    let mut last = 0;

    for range in ranges {
        if range.start > last
            && let Some(plain) = title.get(last..range.start)
        {
            spans.push(span(plain.to_string()));
        }
        if let Some(matched) = title.get(range.clone()) {
            spans.push(span(matched.to_string()).color(hit).font(hit_font));
        }
        last = range.end;
    }
    if let Some(rest) = title.get(last..) {
        spans.push(span(rest.to_string()));
    }

    rich_text(spans).size(13).into()
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
        .map(|(row, (note_index, m))| {
            let note = &app.notes[*note_index];
            let is_selected = row == palette.selected;

            container(
                iced::widget::column![
                    highlighted_title(&note.title, &m.ranges),
                    text(note.folder.clone()).size(10).color(KASUMI),
                ]
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
            scrollable(column(rows).spacing(1)).height(Length::Fixed(360.0)),
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

    // スクリムは背景側だけを覆う層として敷き、その上にパネルを重ねる。
    // パネルごと `mouse_area` で包むと、パネル内のクリックまで「背景クリック」として拾う。
    let scrim = mouse_area(
        container(iced::widget::Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_theme: &iced::Theme| {
                // 濃墨よりさらに深い墨でぼかす。真っ黒ではなく地の色相を保つ。
                container::background(Color::from_rgba8(0x0A, 0x09, 0x0B, 0.6))
            }),
    )
    .on_press(Message::PaletteClose);

    stack![scrim, container(panel).center_x(Fill).padding(80)].into()
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

    let scrim = mouse_area(
        container(iced::widget::Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_theme: &iced::Theme| {
                // 濃墨よりさらに深い墨でぼかす。真っ黒ではなく地の色相を保つ。
                container::background(Color::from_rgba8(0x0A, 0x09, 0x0B, 0.6))
            }),
    )
    .on_press(Message::RenameCancel);

    stack![scrim, container(panel).center_x(Fill).padding(120)].into()
}

fn view(app: &App) -> Element<'_, Message> {
    let t0 = Instant::now();

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
    } else if app.dirty {
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

    let error: Element<'_, Message> = match &app.error {
        // エラーも枯茶。赤を持ち込まない（ADR-0009）。
        Some(message) => text(format!("⚠ {message}")).size(11).color(KARACHA).into(),
        None => text("").size(11).into(),
    };

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
    let root = match vault_root() {
        Ok(root) => root,
        Err(msg) => {
            eprintln!("haboku: {msg}");
            return ExitCode::from(2);
        }
    };

    // 読み込みは iced を起動する前に済ませる。時間を測るのに UI の初期化を混ぜたくない。
    let t0 = Instant::now();
    let notes = vault::load_dir(&root);
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("vault: {} notes / {load_ms:.1}ms / {}", notes.len(), root.display());

    // テーマは一度だけ組む。`theme()` は描画のたびに呼ばれるので、そこで
    // `Theme::custom`（Arc 生成 + extended palette の導出）を回さない。
    let theme = yugen_theme();

    let result = iced::application(
        move || boot(root.clone(), notes.clone(), load_ms),
        update,
        view,
    )
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

/// 開く vault のルートを決める。
///
/// **見つからないときは黙って別の場所を開かない。** 借り物の vault を指したつもりで
/// 空の既定ディレクトリが開くと「ノートが消えた」ようにしか見えず、
/// そのまま書き始めると本当に別の場所へ書き込む。ADR-0002（読み込み専用モードの廃止）と
/// 同じ判断で、危ういフォールバックより即座に失敗するほうを採る。
fn vault_root() -> Result<PathBuf, String> {
    let raw = std::env::var("VAULT")
        .map_err(|_| "環境変数 VAULT に vault のパスを指定してください".to_string())?;
    let root = PathBuf::from(raw);
    if !root.is_dir() {
        return Err(format!("vault が見つかりません: {}", root.display()));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 使い捨て vault を作って `App` を組み立てる。
    ///
    /// **実データを自動保存の実験台にしない。** 本物のノートが壊れる。
    fn app_with_vault(name: &str) -> (PathBuf, App) {
        let dir = std::env::temp_dir().join(format!("haboku-app-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // mtime 降順で並ぶので、後に書いたほうが index 0 に来る。
        for (i, title) in ["古い", "新しい"].iter().enumerate() {
            let body = format!("---\ntitle: \"{title}\"\n---\n\n# {title}\n\n日本語の本文。\n");
            std::fs::write(dir.join(format!("note-{i}.md")), body).unwrap();
        }

        let notes = vault::load_dir(&dir);
        assert_eq!(notes.len(), 2);
        let app = boot(dir.clone(), notes, 0.0);
        (dir, app)
    }

    /// テストからメッセージを流す。`update` が返す `Task` は iced ランタイムへ返すためのもので、
    /// ここでは実行するものが無いので捨てる。
    fn send(app: &mut App, message: Message) {
        let _ = update(app, message);
    }

    /// フォルダ違いのノートを持つ vault を作る。`(フォルダ, タイトル)` の順で置き、
    /// 後に書いたものほど新しい = 一覧の上に来る。
    fn app_with_folders(name: &str, entries: &[(&str, &str)]) -> (PathBuf, App) {
        let dir = std::env::temp_dir().join(format!("haboku-app-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        for (i, (folder, title)) in entries.iter().enumerate() {
            let sub = dir.join(folder);
            std::fs::create_dir_all(&sub).unwrap();
            let body = format!("---\ntitle: \"{title}\"\n---\n\n本文。\n");
            std::fs::write(sub.join(format!("n-{i}.md")), body).unwrap();
        }

        let notes = vault::load_dir(&dir);
        let app = boot(dir.clone(), notes, 0.0);
        (dir, app)
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
        let (dir, mut app) = app_with_vault("writes");
        send(&mut app,Message::NoteSelected(0));
        send(&mut app,insert('X'));
        assert!(app.dirty, "編集したのに dirty が立っていない");

        send(&mut app,Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));

        assert_eq!(app.saves, 1, "デバウンス経過後に保存されていない");
        assert!(!app.dirty, "保存後も dirty が残っている");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(on_disk.contains('X'), "編集がディスクに届いていない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **何も編集せずノートを切り替えたら、ディスクに書かないこと。**
    ///
    /// mtime が動くと外部ツール（git・同期）から「更新された」と誤認される。
    /// Done の定義に明記された禁止事項。
    #[test]
    fn switching_without_editing_does_not_touch_the_file() {
        let (dir, mut app) = app_with_vault("no-write");
        send(&mut app,Message::NoteSelected(0));
        let before = mtime(&app.notes[0].path);

        send(&mut app,Message::NoteSelected(1));

        assert_eq!(app.saves, 0, "無編集なのに書き込んでいる");
        assert_eq!(mtime(&app.notes[0].path), before, "無編集なのに mtime が動いた");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **保存に失敗したらノート切替を中断し、編集内容をエディタに残すこと。**
    ///
    /// ここで進むと、失われた本文はディスクにもメモリにも残らない。前身でデータ喪失を
    /// 招いた欠陥がこれ。失敗系は失敗させないと検証できないので、実際に書けなくする。
    #[test]
    fn failed_save_blocks_the_switch_and_keeps_the_text() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_vault("save-fails");
        send(&mut app,Message::NoteSelected(0));
        send(&mut app,insert('X'));

        // ディレクトリを書き込み不可にする。`vault::save` は一時ファイルを作ってから
        // rename するので、作成の時点で失敗する。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        send(&mut app,Message::NoteSelected(1));

        assert_eq!(app.selected, Some(0), "保存に失敗したのに切り替わった");
        assert!(app.error.is_some(), "保存失敗が表に出ていない");
        assert!(app.dirty, "保存できていないのに dirty が畳まれている");
        assert!(
            app.content.text().contains('X'),
            "未保存の編集内容がエディタから消えた"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **ウィンドウを閉じるときに、デバウンス途中の編集が書き戻されること。**
    ///
    /// 切替・作成・リネーム・削除には保存ガードがあったのに、「閉じる」だけが
    /// 素通りしていた。`Cmd+W` を打った瞬間に直前 1 秒分が消えるので、
    /// 一番踏みやすいデータ喪失の穴だった。
    #[test]
    fn closing_the_window_saves_pending_edits() {
        let (dir, mut app) = app_with_vault("close-saves");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        assert!(app.dirty, "編集したのに dirty が立っていない");

        // デバウンス（1 秒）を待たずに閉じる。ここが実際の操作と同じ条件。
        let _ = close_window(&mut app, iced::window::Id::unique());

        assert_eq!(app.saves, 1, "閉じるときに保存されていない");
        assert!(!app.dirty, "保存したのに dirty が残っている");
        let on_disk = std::fs::read_to_string(&app.notes[0].path).unwrap();
        assert!(on_disk.contains('X'), "閉じる直前の編集がディスクに届いていない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **保存に失敗したら、1 回目は閉じないこと。** 黙って終了すると、切替を中断してまで
    /// 守った本文が最後の一歩で消える。
    ///
    /// Task は中身を覗けないので、「閉じなかった」ことは踏みとどまった痕跡
    /// （dirty が残る・エラーが出る・未保存マーカーが立つ）で確かめる。
    #[test]
    fn first_close_attempt_is_refused_when_saving_fails() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_vault("close-blocked");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _ = close_window(&mut app, iced::window::Id::unique());

        assert!(app.dirty, "保存できていないのに dirty が畳まれている");
        assert!(app.show_marker, "閉じられない理由が画面に出ていない");
        assert!(app.content.text().contains('X'), "未保存の編集がエディタから消えた");
        // **次に何が起きるかまで伝わっていること。** 「閉じられない」だけでは打つ手がない。
        let error = app.error.as_deref().unwrap_or_default();
        assert!(error.contains("退避"), "2 回目に何が起きるかが伝わっていない: {error}");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **2 回目の要求では、本文を退避してから閉じること。**
    ///
    /// 踏みとどまり続けると強制終了しか出口が無くなり、結局全部失う。保存が失敗する原因
    /// （容量・ドライブ・権限）は待っても直らないので、2 回目は必ず閉じる。ただし黙っては捨てない。
    ///
    /// ノートのあるフォルダだけを書けなくして、**退避先が vault ルートへ落ちる**ことも同時に見る。
    #[test]
    fn second_close_attempt_rescues_the_text_before_closing() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_folders("close-rescue", &[("topics", "消えては困る")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        let id = iced::window::Id::unique();
        let _ = close_window(&mut app, id); // 1 回目: 断る
        let _ = close_window(&mut app, id); // 2 回目: 退避して閉じる

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
            vault::load_dir(&dir).iter().all(|n| n.path != rescued[0]),
            "退避ファイルが一覧に出ている"
        );

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **保存が通ったら「一度断った」記憶を畳むこと。**
    ///
    /// 立てっぱなしだと、問題が直ったあとに別の失敗を踏んだとき、**警告なしで**
    /// 1 回目の要求がそのまま退避＋終了になる。2 段階にした意味が消える。
    #[test]
    fn a_successful_save_resets_the_refusal() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_folders("close-reset", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _ = close_window(&mut app, iced::window::Id::unique());
        assert!(app.close_refused, "1 回目を断った記録が残っていない");

        // 権限を直して保存が通れば、記憶は畳まれる。
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        send(&mut app, Message::Tick(Instant::now() + AUTOSAVE_DEBOUNCE));

        assert_eq!(app.saves, 1, "権限を戻したのに保存されていない");
        assert!(!app.close_refused, "保存が通ったのに断った記憶が残っている");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **未保存マーカーが実際に点灯すること。**
    ///
    /// 以前は `should_save` の `else if` に置かれていて、`DIRTY_MARKER_DELAY` >
    /// `AUTOSAVE_MAX_WAIT` である以上**原理的に到達しなかった**。
    /// 「異常のシグナル」と doc に書かれた安全装置が、異常時に沈黙していた。
    #[test]
    fn dirty_marker_lights_up_when_saving_keeps_failing() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_vault("marker");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        let edited = Instant::now();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // 上限（2.5 秒）は超えたがマーカー（3 秒）には届かない時点。まだ黙っている。
        send(&mut app, Message::Tick(edited + AUTOSAVE_MAX_WAIT));
        assert!(app.dirty, "保存に失敗したのに dirty が畳まれた");
        assert!(!app.show_marker, "マーカーの時間に届く前に点灯した");

        // マーカーの時間を越えたら点灯する。
        send(&mut app, Message::Tick(edited + DIRTY_MARKER_DELAY));
        assert!(app.show_marker, "保存できていないのに「未保存」が出ない");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正常に保存できているうちはマーカーを出さないこと。
    /// 常時点灯したらシグナルとして役に立たない。
    #[test]
    fn dirty_marker_stays_off_while_saves_succeed() {
        let (dir, mut app) = app_with_vault("marker-quiet");
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));
        let edited = Instant::now();

        send(&mut app, Message::Tick(edited + DIRTY_MARKER_DELAY));

        assert_eq!(app.saves, 1);
        assert!(!app.show_marker, "保存できているのに「未保存」が出た");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **捨てる前に書き戻すこと。** `.trash` に残るのが「最後に自動保存された内容」だと、
    /// 誤って消したときに直前に打った分だけが戻せない。
    #[test]
    fn deleting_saves_the_pending_edits_into_trash() {
        let (dir, mut app) = app_with_folders("delete-saves", &[("topics", "消すほう")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        send(&mut app, Message::DeleteNote);

        assert_eq!(app.notes.len(), 0, "一覧から消えていない");
        let trashed: Vec<_> = std::fs::read_dir(dir.join(".trash"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(trashed.len(), 1, ".trash に退避されていない");
        let body = std::fs::read_to_string(trashed[0].path()).unwrap();
        assert!(body.contains('X'), "消す直前の編集が .trash に残っていない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 打鍵した直後は書かない。1 文字ごとにディスクへ行ったら意味がない。
    #[test]
    fn does_not_save_right_after_a_keystroke() {
        let now = Instant::now();
        assert!(!should_save(now, now, now));
    }

    /// 手を止めて 1 秒で書く（デバウンス）。
    #[test]
    fn saves_after_debounce() {
        let start = Instant::now();
        let now = start + AUTOSAVE_DEBOUNCE;
        assert!(should_save(now, start, start));
    }

    /// **打ち続けていても上限で書く。** ここが無いと、打鍵が止まらない限り永久に保存されない。
    /// 最後の打鍵は 1ms 前（デバウンスは未達）でも、dirty から 2.5 秒経っていれば書く。
    #[test]
    fn saves_at_max_wait_even_while_typing() {
        let dirty_since = Instant::now();
        let now = dirty_since + AUTOSAVE_MAX_WAIT;
        let last_edit = now - Duration::from_millis(1);
        assert!(should_save(now, last_edit, dirty_since));
    }

    /// 上限に達する寸前・打鍵直後なら、まだ書かない（境界の下側）。
    #[test]
    fn holds_off_just_before_max_wait() {
        let dirty_since = Instant::now();
        let now = dirty_since + AUTOSAVE_MAX_WAIT - Duration::from_millis(1);
        let last_edit = now - Duration::from_millis(1);
        assert!(!should_save(now, last_edit, dirty_since));
    }

    /// 新規ノートは**開いているノートと同じフォルダ**に作られ、一覧の先頭に出ること。
    #[test]
    fn new_note_lands_next_to_the_open_note_and_appears_first() {
        let (dir, mut app) =
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **作成したらサイドバーのフォルダ件数も増えること。**
    ///
    /// `folders` は起動時に 1 回数えるだけだったので、作っても数字が動かなかった。
    /// `notes` から導かれる状態を更新し忘れる類の食い違いなので、回帰テストとして残す。
    #[test]
    fn creating_a_note_updates_the_folder_count() {
        let (dir, mut app) =
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **先頭への差し込みで既存インデックスがずれても、開いているノートがすり替わらないこと。**
    ///
    /// `selected` の付け替えを忘れると、作成した瞬間に隣のノートを編集し始める。
    /// 再現条件が分かりにくいので回帰テストとして残す。
    #[test]
    fn creating_a_note_does_not_swap_the_previously_open_note() {
        let (dir, mut app) =
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同一秒に連打しても両方残ること（ファイル名は秒精度）。
    #[test]
    fn creating_twice_in_the_same_second_keeps_both() {
        let (dir, mut app) = app_with_folders("same-second", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));

        send(&mut app, Message::NewNote);
        let first = app.notes[0].path.clone();
        send(&mut app, Message::NewNote);
        let second = app.notes[0].path.clone();

        assert_ne!(first, second, "同じパスを 2 回使っている");
        assert!(first.exists() && second.exists(), "先に作ったファイルが消えた");
        assert_eq!(app.notes.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **保存に失敗したら新規作成もしないこと。** 作成もエディタを上書きする操作。
    #[test]
    fn new_note_is_blocked_when_saving_fails() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) = app_with_folders("new-note-blocked", &[("topics", "あ")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        send(&mut app, Message::NewNote);

        assert_eq!(app.notes.len(), 1, "保存に失敗したのにノートが増えた");
        assert!(app.content.text().contains('X'), "編集内容が消えた");
        assert!(app.error.is_some());

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **絞り込み中は、開いているノートのフォルダより絞り込みが優先されること。**
    ///
    /// 逆にすると「`notes` を見ているのに `topics` に作られ、それを見せるために
    /// 一覧がすべてに戻る」という動きになる（実際に触って直した）。
    #[test]
    fn creating_while_filtered_uses_that_folder_and_keeps_the_filter() {
        let (dir, mut app) =
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let (dir, mut app) = app_with_folders("rename", &[("topics", "設計メモ")]);
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **同名が居たら枝番へ逃がし、既存を絶対に潰さないこと。**
    #[test]
    fn rename_into_an_existing_name_gets_a_suffix() {
        let (dir, mut app) =
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一覧から消える名前は、エラーを出して**入力欄を開いたまま**にすること。
    /// 閉じてしまうと打ち直せない。
    #[test]
    fn rejected_rename_keeps_the_input_open() {
        let (dir, mut app) = app_with_folders("rename-rejected", &[("topics", "設計メモ")]);
        send(&mut app, Message::NoteSelected(0));
        let before = app.notes[0].path.clone();

        send(&mut app, Message::RenameStarted);
        send(&mut app, Message::RenameChanged(".secret".to_string()));
        send(&mut app, Message::RenameCommit);

        assert!(app.rename.is_some(), "打ち直せない");
        assert!(app.error.is_some(), "理由が表に出ていない");
        assert_eq!(app.notes[0].path, before, "拒否したのにリネームされた");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(app.selected, None, "消したノートが開いたままになっている");
        assert_eq!(
            app.folders.iter().find(|(n, _)| n == "topics").unwrap().1,
            1,
            "フォルダ件数が減っていない"
        );

        // ファイルとしては .trash に残っている（Finder で戻せる）。
        let trashed: Vec<_> = std::fs::read_dir(dir.join(".trash")).unwrap().collect();
        assert_eq!(trashed.len(), 1, ".trash に退避されていない");
        let _ = std::fs::remove_dir_all(&dir);
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
        let root = vault_root().expect("VAULT に実データの vault を指定して実行する");
        let notes = vault::load_dir(&root);
        let count = notes.len();
        let mut app = boot(root, notes, 0.0);

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
        let (dir, app) = app_with_folders("truncate", &refs);

        assert_eq!(refilter(&app.notes, "").len(), PALETTE_MAX_RESULTS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **パレットは絞り込み中のフォルダを無視して全ノートを探し、開いたら絞り込みを解除すること。**
    ///
    /// 解除しないと「選択中のノートが左の一覧に無い」状態が生まれる。
    #[test]
    fn palette_opens_a_note_from_another_folder_and_clears_the_filter() {
        let (dir, mut app) =
            app_with_folders("cross-folder", &[("topics", "設計メモ"), ("notes", "走り書き")]);

        send(&mut app, Message::FolderSelected(Some("notes".to_string())));
        assert_eq!(app.visible.len(), 1, "フォルダ絞り込みが効いていない");

        // 絞り込み対象外（topics）のノートを検索して開く。
        open_palette(&mut app, "設計");
        assert_eq!(app.palette.as_ref().unwrap().matches.len(), 1);
        handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());

        let opened = app.selected.expect("ノートが開かれていない");
        assert_eq!(app.notes[opened].title, "設計メモ");
        assert!(app.palette.is_none(), "開いたのにパレットが残っている");
        assert!(app.selected_folder.is_none(), "絞り込みが解除されていない");
        assert!(
            app.visible.contains(&opened),
            "開いたノートが一覧に見えていない"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **絞り込みの中のノートをパレットから開いたときは、絞り込みを維持すること。**
    ///
    /// 以前は無条件に解除していて「topics で絞り込んで topics のノートを開いたのに
    /// すべてに戻る」動きになっていた。解除は「開いたノートが一覧から消える」ときだけの手段。
    #[test]
    fn palette_keeps_the_filter_when_the_note_is_already_visible() {
        let (dir, mut app) = app_with_folders(
            "palette-keeps-filter",
            &[("topics", "設計メモ"), ("topics", "別の設計"), ("notes", "走り書き")],
        );
        send(&mut app, Message::FolderSelected(Some("topics".to_string())));

        open_palette(&mut app, "設計メモ");
        handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());

        assert_eq!(app.notes[app.selected.unwrap()].title, "設計メモ");
        assert_eq!(
            app.selected_folder.as_deref(),
            Some("topics"),
            "同じフォルダのノートを開いただけで絞り込みが解除された"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **保存に失敗したらパレットからも遷移しないこと。** 一覧クリックと同じガードが要る。
    /// パレットは閉じない（閉じてから中断すると、なぜ切り替わらないのかが分からなくなる）。
    #[test]
    fn palette_enter_is_blocked_when_saving_fails() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, mut app) =
            app_with_folders("palette-save-fails", &[("topics", "あ"), ("topics", "い")]);
        send(&mut app, Message::NoteSelected(0));
        send(&mut app, insert('X'));

        let sub = dir.join("topics");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();

        open_palette(&mut app, "");
        // 先頭以外を選んでから Enter（自分自身を開き直すのでは検証にならない）。
        app.palette.as_mut().unwrap().selected = 1;
        handle_palette_key(&mut app, &named(keyboard::key::Named::Enter), <_>::default());

        assert_eq!(app.selected, Some(0), "保存に失敗したのに切り替わった");
        assert!(app.palette.is_some(), "中断したのにパレットが閉じた");
        assert!(app.error.is_some(), "保存失敗が表に出ていない");
        assert!(app.content.text().contains('X'), "編集内容が消えた");

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ctrl-n / ctrl-p が循環すること。矢印キーは `text_input` に消費されて届かない。
    #[test]
    fn palette_selection_cycles_with_ctrl_n_and_ctrl_p() {
        let (dir, mut app) =
            app_with_folders("cycle", &[("topics", "あ"), ("topics", "い"), ("topics", "う")]);
        open_palette(&mut app, "");
        assert_eq!(app.palette.as_ref().unwrap().matches.len(), 3);

        let ctrl = keyboard::Modifiers::CTRL;
        handle_palette_key(&mut app, &key("n"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 1);
        handle_palette_key(&mut app, &key("p"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 0);
        // 先頭で戻ると末尾へ回り込む。
        handle_palette_key(&mut app, &key("p"), ctrl);
        assert_eq!(app.palette.as_ref().unwrap().selected, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Escape で閉じること（`Cmd+P` のトグルが使えないので、閉じ方はこちらに寄せている）。
    #[test]
    fn palette_closes_on_escape() {
        let (dir, mut app) = app_with_folders("escape", &[("topics", "あ")]);
        open_palette(&mut app, "");
        handle_palette_key(&mut app, &named(keyboard::key::Named::Escape), <_>::default());
        assert!(app.palette.is_none());
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(!should_save(now, last_edit, dirty_since_wrongly_reset));
    }
}
