//! haboku 本体。3 ペイン（フォルダ / 一覧 / エディタ）+ デバウンス自動保存。
//!
//! 実行: `VAULT="$HOME/Documents/haboku" cargo run --release`
//!
//! **必ず release で動かすこと。** debug だと vault 読み込みの数字が一桁変わる
//! （実測: cold 185.1ms / warm 32.8ms @ release・約 1200 件）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, row, scrollable, text, text_editor};
use iced::{Element, Fill, Font, Length, Subscription, Task};

use haboku::vault;

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

/// サイドバー（フォルダ）の幅。
const SIDEBAR_WIDTH: f32 = 180.0;
/// ノート一覧の幅。
const LIST_WIDTH: f32 = 320.0;

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
            "左の一覧からノートを選ぶと、ここに本文が出ます。",
        ),
        dirty: false,
        dirty_since: None,
        last_edit: Instant::now(),
        show_marker: false,
        error: None,
        saved_flash_until: None,
        saves: 0,
        load_ms,
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
    }
    Task::none()
}

/// **dirty（か、消すべき表示がある）ときだけタイマーを回す。**
///
/// `Subscription` は state を見て出し分けられるので、何も編集していない間はタイマーが
/// そもそも存在しない。常時ポーリングにならずに済む。
fn subscription(app: &App) -> Subscription<Message> {
    if app.dirty || app.saved_flash_until.is_some() {
        iced::time::every(TICK).map(Message::Tick)
    } else {
        Subscription::none()
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

fn view(app: &App) -> Element<'_, Message> {
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
    column![
        panes,
        container(status).height(Length::Fixed(18.0)),
        container(error).height(Length::Fixed(16.0)),
    ]
    .spacing(6)
    .padding(8)
    .into()
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

    fn insert(c: char) -> Message {
        Message::Edit(text_editor::Action::Edit(text_editor::Edit::Insert(c)))
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
