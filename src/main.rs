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
        dirty: false,
        dirty_since: None,
        last_edit: Instant::now(),
        show_marker: false,
        error: None,
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
            // 「選択中のノートが左の一覧に無い」状態になるため、絞り込みを解除して
            // 開いたノートが必ず一覧に見えるようにする。
            if app.selected_folder.is_some() {
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

            if let Some(dirty_since) = app.dirty_since {
                if should_save(now, app.last_edit, dirty_since) {
                    save_now(app);
                } else if now.duration_since(dirty_since) >= DIRTY_MARKER_DELAY {
                    // ここへ到達すること自体が「自動保存が動いていない」シグナル。
                    app.show_marker = true;
                }
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

    if app.dirty || app.saved_flash_until.is_some() {
        Subscription::batch([keys, iced::time::every(TICK).map(Message::Tick)])
    } else {
        keys
    }
}

fn folder_pane(app: &App) -> Element<'_, Message> {
    let all = button(
        row![
            text("すべて").size(12).width(Fill),
            text(app.notes.len().to_string()).size(12),
        ]
        .padding(2),
    )
    .on_press(Message::FolderSelected(None))
    .width(Fill)
    .style(if app.selected_folder.is_none() {
        button::primary
    } else {
        button::text
    });

    let items = app.folders.iter().map(|(name, count)| {
        let selected = app.selected_folder.as_deref() == Some(name.as_str());
        button(
            row![
                text(name).size(12).width(Fill),
                text(count.to_string()).size(12),
            ]
            .padding(2),
        )
        .on_press(Message::FolderSelected(Some(name.clone())))
        .width(Fill)
        .style(if selected {
            button::primary
        } else {
            button::text
        })
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
        let body = column![
            text(&note.title).size(13),
            text(&note.preview).size(10),
        ]
        .spacing(2);

        button(body)
            .on_press(Message::NoteSelected(index))
            .width(Fill)
            .style(if app.selected == Some(index) {
                button::primary
            } else {
                button::text
            })
            .into()
    });

    scrollable(column(rows).spacing(1)).height(Fill).into()
}

fn editor_pane(app: &App) -> Element<'_, Message> {
    text_editor(&app.content)
        .on_action(Message::Edit)
        .font(EDITOR_FONT)
        // 日本語には単語境界がほぼ無く、既定の `Word` だと長い段落が 1 つの巨大な単語になる。
        .wrapping(iced::advanced::text::Wrapping::WordOrGlyph)
        .highlight("md", iced::highlighter::Theme::Base16Ocean)
        .height(Fill)
        .into()
}

/// マッチした文字だけ色を変えたタイトルを作る。
///
/// `fuzzy::Match::ranges` は**バイト範囲**なので `get()` で受ける。日本語タイトルで
/// 文字境界を跨いだときに panic しないため（`&title[range]` だと落ちる）。
fn highlighted_title(title: &str, ranges: &[std::ops::Range<usize>]) -> Element<'static, Message> {
    let hit = Color::from_rgb(1.0, 0.78, 0.25);
    let mut spans: Vec<iced::advanced::text::Span<'static, ()>> = Vec::new();
    let mut last = 0;

    for range in ranges {
        if range.start > last
            && let Some(plain) = title.get(last..range.start)
        {
            spans.push(span(plain.to_string()));
        }
        if let Some(matched) = title.get(range.clone()) {
            spans.push(span(matched.to_string()).color(hit));
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
                    text(note.folder.clone()).size(10),
                ]
                .spacing(1),
            )
            .padding(6)
            .width(Fill)
            .style(move |theme: &iced::Theme| {
                if is_selected {
                    container::background(theme.extended_palette().primary.weak.color)
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
            .size(10),
        ]
        .spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(640.0))
    .style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.base.color.into()),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..container::Style::default()
        }
    });

    // スクリムは背景側だけを覆う層として敷き、その上にパネルを重ねる。
    // パネルごと `mouse_area` で包むと、パネル内のクリックまで「背景クリック」として拾う。
    let scrim = mouse_area(
        container(iced::widget::Space::new().width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(|_theme: &iced::Theme| {
                container::background(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
            }),
    )
    .on_press(Message::PaletteClose);

    stack![scrim, container(panel).center_x(Fill).padding(80)].into()
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

    let status = row![
        text(format!("{} notes", app.visible.len())).size(11),
        text(format!("load {:.1}ms", app.load_ms)).size(11),
        // Done の定義は 1ms 未満。ここが太りだしたら `view()` に重い処理が入った合図。
        text(format!(
            "view {:.2}ms",
            app.last_view_us.get() as f64 / 1000.0
        ))
        .size(11),
        // 編集していないのに増えるなら「無編集でも書いている」ということ。
        text(format!("saves {}", app.saves)).size(11),
        text(if app.show_marker {
            "● 未保存"
        } else if app.dirty {
            "… 編集中"
        } else if app.saved_flash_until.is_some() {
            "保存しました"
        } else {
            "―"
        })
        .size(11),
    ]
    .spacing(16);

    let error: Element<'_, Message> = match &app.error {
        Some(message) => text(format!("⚠ {message}"))
            .size(11)
            .color(iced::Color::from_rgb(1.0, 0.45, 0.4))
            .into(),
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

    let element = match &app.palette {
        Some(palette) => stack![base, palette_overlay(app, palette)].into(),
        None => base,
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

    let result = iced::application(
        move || boot(root.clone(), notes.clone(), load_ms),
        update,
        view,
    )
    .title("haboku")
    .subscription(subscription)
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
