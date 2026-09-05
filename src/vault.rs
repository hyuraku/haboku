//! 保存層。方針は「`.md` が唯一の真実」（決定: 2026-08-06 / 案2）。
//!
//! SQLite は将来ここに足す全文検索インデックスだが、それは常に再構築可能な
//! キャッシュであって真実ではない。Boostnote が `.cson` を真実にしたせいで
//! 移行コストを払う羽目になった、その轍を踏まないための線引き。
//!
//! STEP 2 の時点では読み込みのみ。書き戻しは STEP 3。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// サブフォルダに属さない（vault ルート直下に置かれた）ノートのフォルダ名。
pub const ROOT_FOLDER: &str = "/";

#[derive(Debug, Clone)]
pub struct Note {
    pub path: PathBuf,
    pub title: String,
    pub tags: Vec<String>,
    /// vault ルート直下のディレクトリ名（`sources` / `topics` など）。
    /// 直下に置かれたノートは [`ROOT_FOLDER`]。
    /// 実データは tags をほぼ持たず、分類はフォルダが担っていた。
    pub folder: String,
    /// 一覧に出す1行。summary が無ければ、タイトルに使った行より後の本文から取る。
    pub preview: String,
    /// frontmatter を含むファイル全文。エディタにはこれをそのまま渡す。
    pub raw: String,
    /// 最終更新日時。一覧の既定順（新しい順）に使う。
    pub modified: SystemTime,
}

/// 読み込みの結果。**読めたノートと、読めなかった件数の両方を返す。**
///
/// ノートだけを返していると、呼び出し元は「読めなかった」を知る手段が無い。
/// stderr へ書くだけでは `.app` の利用者には届かず、**権限や I/O エラーで
/// 欠けた一覧が「そういう vault」に見える**（ADR-0002 が避けたかった状態と同型）。
#[derive(Debug, Clone, Default)]
pub struct Load {
    pub notes: Vec<Note>,
    /// 読めなかった件数。**意図的に飛ばしたものは数えない**
    /// （dot 始まり・`node_modules`・symlink は仕様どおりの除外で、失敗ではない）。
    /// 数えると「毎回 1 件読めていない」と言い続ける狼少年になる。
    pub failed: usize,
    /// 最初の失敗の理由だけ持つ。全部持つと病的な vault で膨らむ上、
    /// 画面に出せるのは 1 行なので、原因の見当が付く例示に足りればよい。
    pub first_failure: Option<String>,
}

impl Load {
    fn failed(&mut self, what: String) {
        // 端末から起動していれば全件の詳細はここに出る。画面には件数と 1 件目だけ。
        eprintln!("vault: {what}");
        self.failed += 1;
        if self.first_failure.is_none() {
            self.first_failure = Some(what);
        }
    }
}

/// `root` 以下の `.md` を再帰的に読む。
///
/// 依存を増やさないため walkdir は使わず std だけで歩く。検証段階では
/// これで十分で、ファイル監視が要るようになった時点で notify に載せ替える。
pub fn load_dir(root: &Path) -> Load {
    let mut load = Load::default();
    walk(root, root, &mut load);
    sort_notes(&mut load.notes);
    load
}

/// 一覧の既定順: **最近いじった順**（Boostnote の体感に近い）。
///
/// gpui 版では「mtime は毎回 stat するとコスト」として title 順で妥協していたが、
/// どのみち全ファイルを読んでいるので、開いたハンドルから mtime を取れば
/// 追加コストは `fstat` だけで済む（実測は `docs/implementation-notes.md`）。
///
/// 同時刻の場合はタイトル順にする。一括生成・git checkout で mtime が揃うことがあり、
/// そこで順序が実行ごとに変わると「一覧が勝手に踊る」ように見える。
fn sort_notes(notes: &mut [Note]) {
    notes.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.title.cmp(&b.title)));
}

fn walk(root: &Path, dir: &Path, load: &mut Load) {
    // 読めないものは黙って飛ばさない。無言だと「空 vault」と「権限などで
    // 読めていない」の区別が付かず、原因調査ができなくなる。
    // **件数は `Load` で呼び出し元へ返す**（画面に出すのは main の仕事）。
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            load.failed(format!("{} を開けません（{e}）", dir.display()));
            return;
        }
    };
    // **`flatten()` は使わない。** エントリ単位の失敗（readdir の途中エラー）を
    // 完全に捨ててしまい、何件消えたのかが誰にも分からなくなる。
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                load.failed(format!("{} の一覧を取れません（{e}）", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // .git や node_modules に潜っても意味がないので弾く
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }

        // **symlink は辿らない。`path.is_dir()` ではなく `entry.file_type()` で見る。**
        //
        // `is_dir()` はリンクを解決するので、vault の外を指すディレクトリ symlink を
        // 踏むと外のファイルを一覧に載せてしまう。載れば編集でき、**保存も削除も
        // vault の外で起きる**（`vault_root` の「黙って別の場所を開かない」と同じ危険）。
        // 加えて `a -> ..` のような循環でスタックを食い潰すまで再帰する。
        // `file_type()` は readdir が返した種別なのでリンクを解決しない。
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(e) => {
                load.failed(format!("{} の種別が取れません（{e}）", path.display()));
                continue;
            }
        };
        if kind.is_symlink() {
            // 仕様どおりの除外なので**失敗には数えない**（`Load::failed` の doc 参照）。
            eprintln!("vault: skip symlink {}", path.display());
            continue;
        }

        if kind.is_dir() {
            walk(root, &path, load);
        } else if path.extension().is_some_and(|e| e == "md") {
            match read_with_mtime(&path) {
                Ok((raw, modified)) => load.notes.push(parse_note(root, path, raw, modified)),
                Err(e) => load.failed(format!("{} を読めません（{e}）", path.display())),
            }
        }
    }
}

/// 本文と mtime を **1 回のパス解決で** 取る。
///
/// `std::fs::metadata(path)` を別に呼ぶとディレクトリ解決をもう一度やり直すことになる。
/// どのみち開いて読むのだから、開いたハンドルから metadata を取れば追加コストは `fstat` だけ。
/// 約 1200 件でこの差が起動時間の予算（50ms）に効く。
fn read_with_mtime(path: &Path) -> std::io::Result<(String, SystemTime)> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let modified = file.metadata()?.modified()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    Ok((raw, modified))
}

/// 保存。**書き込み中に落ちても元ファイルを壊さない**ことを最優先にする。
///
/// 同じ内容を `fs::write` で直接上書きすると、途中でクラッシュしたときに
/// 半分だけ書かれたファイルが残る。メモアプリでそれは許されないので、
/// 一時ファイルへ書いてから `rename` する。rename は同一ファイルシステム上では
/// アトミックなので、成功か失敗のどちらかにしかならない。
///
/// **rename のアトミック性だけでは足りない。** 中身が実際にディスクへ届く前に
/// rename が先に永続化されると、電源断のあとに「新しい名前で中身が空（またはゴミ）」の
/// ファイルが残る。プロセスが落ちるだけなら OS のページキャッシュが救ってくれるが、
/// 電源断は救ってくれない。だから rename の前に `sync_all` で中身を確定させる。
///
/// **親ディレクトリの fsync は意図的にやらない。** そこまでやると rename 自体の
/// 永続化まで保証できるが、省いても最悪の結果は「古い内容のまま残る」であって
/// 壊れたファイルではない。この関数が守ると宣言しているのは前者だけ。
///
/// **rename は inode ごと差し替えるので、権限は明示的に引き継ぐ**（ADR-0011）。
/// 引き継がないと `0600` で置いた非公開のノートが、保存しただけで umask 由来の
/// `0644` に緩む。**保存という操作がファイルの公開範囲を広げてはいけない。**
/// ACL と xattr は引き継がない（同 ADR の帰結）。
pub fn save(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // **一時ファイル名にプロセス ID を混ぜる。** 固定名（`note.md.tmp`）だと、
    // 同じ vault を 2 つの haboku で開いたときに互いの一時ファイルを踏み合い、
    // 片方の本文がもう片方のノートへ rename され得る。
    // `.md` 以外は `load_dir` が拾わないので、一覧には出ない。
    let tmp = path.with_extension(format!("md.{}.tmp", std::process::id()));

    // 元ファイルの権限。**取れなかったら緩いほうではなく厳しいほう（0600）へ倒す。**
    // ここへ来るのは元ファイルが消えている場合で、既定の umask に任せると
    // 「外部で消えたノートを書き戻したら公開範囲が広がった」が起きる。
    let mode = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o600);

    // ブロックにして、rename より先に必ずファイルを閉じる。
    {
        // **作るときは 0600、中身を書き終えてから元の権限へ広げる。**
        // 先に緩い権限で作ると、書き込んでいる最中の一時ファイルが他ユーザーから
        // 読める窓が開く（元が 0600 なら、その窓は元ファイルには存在しなかったもの）。
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        // sync_all より前に権限を戻す。fsync はメタデータも一緒に確定させるので、
        // 「中身は届いたが権限だけ古い」中途半端な状態を残さない。
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
    }

    std::fs::rename(&tmp, path)
}

/// `dir` 内で `file_name` と衝突しない空きパスを **空ファイルを作って予約し** 返す。
///
/// `save()` の rename は宛先が存在しても黙って置換する。アトミック保存には
/// その性質が必須だが、衝突を避けたい場面（新規作成・`.trash` への移動）で
/// 同じ rename に頼ると後勝ちの上書き事故になる。ここでは `create_new`
/// （存在すれば失敗する）で先にファイルを確保するので、同名の生成が
/// 同一秒に重なっても既存ファイルが消えることはない。
///
/// 衝突時は拡張子の前に `-2`, `-3`, … を挟んだ名前を順に試す。
/// 呼び出し側は返ったパスへ書き込むか rename で上書きする（予約は空ファイル）。
///
/// **予約は必ず `0600` で作る。** 予約ファイルのその後は呼び出し側で二手に分かれる:
///
/// - `move_to` は直後の `rename` で inode ごと差し替えるので、ここの権限は消える
/// - **新規ノートと `.rescue` は予約したその inode に書き続ける**ので、ここの権限が
///   そのまま最終的な公開範囲になる
///
/// 後者を umask 任せにすると、よくある `0022` で `0644` になる。しかも `save()` は
/// 元ファイルの権限を引き継ぐ設計（ADR-0011）なので、新規ノートは**初回の予約で付いた
/// `0644` を以後ずっと引きずる**。`.rescue` に至っては、保存できなかった本文が
/// 他ユーザーから読める場所へ落ちる。ADR-0011 が「保存という操作がファイルの公開範囲を
/// 広げてはいけない」と言うなら、**作成という操作も広げてはいけない**。
///
/// 緩めたい場合は呼び出し側が後から `set_permissions` すればよい。厳しいほうから
/// 始めるのは、逆向き（緩く作って後で締める）だと締めるまでの窓が開くため。
pub fn reserve_unique(dir: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;

    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (file_name, None),
    };
    for n in 1u32.. {
        let candidate = match (n, ext) {
            (1, _) => file_name.to_string(),
            (_, Some(ext)) => format!("{stem}-{n}.{ext}"),
            (_, None) => format!("{stem}-{n}"),
        };
        let path = dir.join(candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("u32 の全候補が埋まることはない")
}

/// ファイルを `dir` の `file_name` へ移す。**衝突は枝番で避け、既存を絶対に置換しない。**
///
/// `std::fs::rename` は宛先が存在しても黙って置換する。`save()` のアトミック保存には
/// この性質が必須だが、**同じ道具を移動に流用すると後勝ちの上書きでノートが消える**
/// （前身のデータ喪失欠陥 3 件の共通の根がこれ）。`reserve_unique` で先に空きを予約し、
/// その予約済みパスへ rename する。返り値は実際の移動先。
pub fn move_to(from: &Path, dir: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    let dest = reserve_unique(dir, file_name)?;
    std::fs::rename(from, &dest)?;
    Ok(dest)
}

/// 保存できなかった本文を、書ける場所へ退避する。返り値は実際の退避先。
///
/// **最後の手段。** 通常の保存経路（`save`）が失敗し、なおユーザーが終了を選んだときだけ通る。
/// ここまで来て黙って捨てると、打った本文はディスクにもメモリにも残らない。
///
/// `dirs` は**優先順**で渡し、書けた最初の場所を使う。保存が失敗する原因（権限・容量・
/// ドライブが外れた）は特定のディレクトリだけで起きるとは限らないので、退避先も 1 つに賭けない。
///
/// 名前は `<元のファイル名>.rescue`。拡張子が `.md` でないので `load_dir` は拾わず、
/// 一覧を汚さないまま Finder からは見える。`reserve_unique` 経由なので**既存を絶対に潰さない**。
pub fn write_rescue(dirs: &[PathBuf], file_name: &str, contents: &str) -> std::io::Result<PathBuf> {
    let name = format!("{file_name}.rescue");
    let mut last: Option<std::io::Error> = None;

    for dir in dirs {
        let path = match reserve_unique(dir, &name) {
            Ok(path) => path,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        match std::fs::write(&path, contents) {
            Ok(()) => return Ok(path),
            Err(e) => {
                // 予約だけ済んで書けなかった空ファイルを残さない。
                // 中身ゼロの `.rescue` は「退避できた」という誤った合図になる。
                let _ = std::fs::remove_file(&path);
                last = Some(e);
            }
        }
    }

    Err(last.unwrap_or_else(|| std::io::Error::other("退避先が 1 つも指定されていない")))
}

/// vault 内の `.trash/` へ退避する。返り値は退避先。
///
/// dot 始まりのディレクトリは `load_dir` の走査から外れるので、退避したノートは
/// 一覧から消えるが**ファイルとしては残る**（Finder で戻せる）。
pub fn move_to_trash(root: &Path, path: &Path) -> std::io::Result<PathBuf> {
    let trash = root.join(".trash");
    std::fs::create_dir_all(&trash)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled.md".to_string());
    move_to(path, &trash, &name)
}

/// 1 件ぶんのメタ情報（タイトル・プレビュー・タグ）を本文から取る。
/// 読み込み時（`walk`）と、保存・リネーム後の取り直しの両方から呼ぶ。
pub fn parse_note(root: &Path, path: PathBuf, raw: String, modified: SystemTime) -> Note {
    let (fm, body) = split_frontmatter(&raw);
    let (directive_title, directive_tags, body) = split_directives(body);
    let mut preview_body = body;

    // frontmatter > 先頭ディレクティブ > 本文の先頭行 > ファイル名。
    let title = fm
        .as_ref()
        .and_then(|f| scalar(f, "title"))
        .or_else(|| directive_title.map(str::to_string))
        .or_else(|| {
            title_from_body(body).map(|(title, end)| {
                preview_body = &body[end..];
                title
            })
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    // 空の tags: も明示指定。キーが無いときだけディレクティブへ進む。
    let tags = fm
        .filter(|f| f.lines().any(|line| line.starts_with("tags:")))
        .map(tags)
        .unwrap_or_else(|| {
            directive_tags
                .filter(|value| !value.starts_with('#'))
                .map(|value| {
                    strip_comment(value)
                        .split(',')
                        .filter_map(clean_tag)
                        .collect()
                })
                .unwrap_or_default()
        });

    // 実データの vault は tags をほぼ持たず、sources / topics / notes …
    // というルート直下のフォルダが分類を担っていた。サイドバーはそれに合わせる。
    let folder = path
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.parent())
        .and_then(|p| p.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| ROOT_FOLDER.to_string());

    // summary が無ければ、ディレクティブとタイトルに消費した行を除いた本文を使う。
    let preview = fm
        .as_ref()
        .and_then(|f| scalar(f, "summary"))
        .unwrap_or_else(|| {
            preview_body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .to_string()
        })
        .chars()
        .take(80)
        .collect();

    Note {
        path,
        title,
        tags,
        folder,
        preview,
        raw,
        modified,
    }
}

/// 先頭の空行と既知のディレクティブだけを消費し、値と残りの本文を借用で返す。
/// 普通の行で打ち切るため、本文やコードブロック内の記法はメタ情報にならない。
fn split_directives(body: &str) -> (Option<&str>, Option<&str>, &str) {
    let mut title = None;
    let mut tags = None;
    let mut end = 0;
    for line in body.split_inclusive('\n') {
        if let Some(value) = line.strip_prefix("#title:") {
            title.get_or_insert(value.trim());
        } else if let Some(value) = line.strip_prefix("#tags:") {
            tags.get_or_insert(value.trim());
        } else if !line.trim().is_empty() {
            break;
        }
        end += line.len();
    }
    (title.filter(|value| !value.is_empty()), tags, &body[end..])
}

/// 本文の 1 行目からタイトルを取る。frontmatter も `#title:` も無いノート用。
///
/// `body` は frontmatter と先頭ディレクティブブロックを除いた残り。
/// 返すのは「タイトルとして採用する文字列」と「消費した行の終端バイト位置」。
/// preview はその位置より後ろから取る。
/// タイトルは最大 80 文字とし、長い行も行末まで消費する。
fn title_from_body(body: &str) -> Option<(String, usize)> {
    let mut end = 0;
    for line in body.split_inclusive('\n') {
        end += line.len();
        let line = line.trim_start();
        let title = line
            .strip_prefix("## ")
            .or_else(|| line.strip_prefix("# "))
            .unwrap_or(line)
            .trim();
        if !title.is_empty() {
            return Some((title.chars().take(80).collect(), end));
        }
    }
    None
}

/// `---` で挟まれた frontmatter を切り出す。無ければ `None`。
///
/// **改行の種類は問わない。** CRLF のファイルをここで弾くと frontmatter 全体が
/// 本文へ流れ込み、タグだけでなく `title` も `summary` も落ちる。取りこぼしの中で
/// これが一番被害が大きい（他のエディタや Windows 由来のファイルで普通に起きる）。
fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let Some(rest) = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
    else {
        return (None, raw);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, raw);
    };
    // CRLF なら `end` は `\r` の直後を指すが、fm も body も行単位で読むので
    // 残った `\r` は `str::lines()` が落とす。
    //
    // **fm は借用のまま返す。** 読まれるだけなので所有権は要らず、
    // `to_string()` にすると 約 1200 件ぶんのコピーが起動時に走る。
    let body = rest[end..]
        .trim_start_matches("\n---")
        .trim_start_matches(['\r', '\n']);
    (Some(&rest[..end]), body)
}

/// YAML の行末コメント（空白のあとの `#`）を落とす。
///
/// **空白を要求する。** `#` の直前が空白でなければコメントではないので、
/// タグとして書かれた `#rust` のような値を削らずに済む。
fn strip_comment(value: &str) -> &str {
    match value.find(" #").or_else(|| value.find("\t#")) {
        Some(at) => &value[..at],
        None => value,
    }
}

/// `key: value` を1つ拾う。YAML パーサは入れない（frontmatter は浅いので過剰）。
fn scalar(fm: &str, key: &str) -> Option<String> {
    // **`format!("{key}:")` を作らない。** クロージャの中なので走査した行数ぶん
    // `String` が確保される。`parse_note` は 1 件につき 3 回ここへ来るので、
    // 約 1200 件では 1 万回規模の無駄になり、起動の 50ms 予算に直接効く。
    fm.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|v| !v.is_empty())
}

/// tags は `tags: [a, b]` と `tags:\n  - a\n  - b` の両方が現場に存在する。
///
/// **括弧は必須にしない。** `tags: rust` や `tags: rust, iced` は YAML として合法で、
/// 実際に書かれる。ここで落とすと「書いたのに反映されない」を黙って作る。
fn tags(fm: &str) -> Vec<String> {
    if let Some(inline) = scalar(fm, "tags") {
        let inner = match inline.strip_prefix('[') {
            // `]` より後ろは行末コメントなど。`]` を書き忘れていても中身は拾う。
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            // `tags:  # あとで書く` は YAML ではコメントだけの行＝タグ無し。
            None if inline.starts_with('#') => "",
            None => strip_comment(&inline),
        };
        return inner.split(',').filter_map(clean_tag).collect();
    }
    let mut out = Vec::new();
    let mut in_tags = false;
    for line in fm.lines() {
        if line.starts_with("tags:") {
            in_tags = true;
            continue;
        }
        if in_tags {
            if let Some(t) = line.trim().strip_prefix("- ") {
                out.extend(clean_tag(t));
            } else if line.trim().is_empty() {
                // 空行では終わらせない。リストの途中に 1 行空けて書く人がいる。
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
        }
    }
    out
}

/// タグ 1 つ分の前後を落とす。空になったものは捨てる。
///
/// 引用符は `"` と `'` の両方。片方だけだと `tags: ['a', 'b']` が
/// `'a'` という別のタグとして残る。
fn clean_tag(raw: &str) -> Option<String> {
    let t = strip_comment(raw).trim().trim_matches(['"', '\'']).trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    /// 順序だけを見たいので、ファイルシステムは触らずに Note を組み立てる。
    /// mtime を実ファイルで作り分けるには filetime クレートか sleep が要り、
    /// 前者は依存が増え、後者はテストが時計に依存して不安定になる。
    fn note(title: &str, secs: u64) -> Note {
        Note {
            path: PathBuf::from(format!("{title}.md")),
            title: title.to_string(),
            tags: Vec::new(),
            folder: "/".to_string(),
            preview: String::new(),
            raw: String::new(),
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs),
        }
    }

    /// 既定順は更新日時の新しい順であること。
    #[test]
    fn sorts_newest_first() {
        let mut notes = vec![note("古い", 100), note("新しい", 300), note("中間", 200)];
        sort_notes(&mut notes);
        let titles: Vec<&str> = notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["新しい", "中間", "古い"]);
    }

    /// 同時刻はタイトル順に倒れること。一括生成や git checkout で mtime が揃ったとき、
    /// 実行ごとに並びが変わると「一覧が勝手に踊る」ように見える。
    #[test]
    fn sorts_same_mtime_by_title() {
        let mut notes = vec![note("b", 100), note("a", 100), note("c", 100)];
        sort_notes(&mut notes);
        let titles: Vec<&str> = notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["a", "b", "c"]);
    }

    /// 空きがあればそのままの名前で予約され、ファイルが実在すること。
    #[test]
    fn reserve_unique_uses_plain_name_when_free() {
        let dir = TempDir::new("reserve-plain");
        let path = reserve_unique(&dir, "a.md").unwrap();
        assert_eq!(path, dir.join("a.md"));
        assert!(path.exists(), "予約したファイルが作られていない");
    }

    /// 同じ名前を続けて予約すると -2, -3 と枝番が付き、既存を一切潰さないこと。
    /// 秒精度ファイル名の Cmd+N 連打・同名ノートの連続削除がこの性質に乗る。
    #[test]
    fn reserve_unique_never_replaces_existing_files() {
        let dir = TempDir::new("reserve-suffix");
        std::fs::write(dir.join("a.md"), "既存の中身").unwrap();

        let second = reserve_unique(&dir, "a.md").unwrap();
        let third = reserve_unique(&dir, "a.md").unwrap();

        assert_eq!(second, dir.join("a-2.md"));
        assert_eq!(third, dir.join("a-3.md"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.md")).unwrap(),
            "既存の中身",
            "既存ファイルが置換された"
        );
    }

    /// 移動先が埋まっていても既存を置換せず、枝番を付けて逃がすこと。
    /// **`fs::rename` を直接使うとここで既存が消える**（前身のデータ喪失欠陥の根）。
    #[test]
    fn move_to_never_replaces_an_existing_file() {
        let dir = TempDir::new("move-collision");
        std::fs::write(dir.join("a.md"), "先客").unwrap();
        std::fs::write(dir.join("b.md"), "移動するほう").unwrap();

        let dest = move_to(&dir.join("b.md"), &dir, "a.md").unwrap();

        assert_eq!(dest, dir.join("a-2.md"));
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "先客");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "移動するほう");
        assert!(!dir.join("b.md").exists(), "移動元が残っている");
    }

    /// `.trash` へ退避すると、ファイルは残るが `load_dir` の走査からは外れること。
    /// 同名を 2 回捨てても先に捨てたほうが消えないこと。
    #[test]
    fn move_to_trash_hides_the_note_but_keeps_the_file() {
        let dir = TempDir::new("trash");
        std::fs::create_dir_all(dir.join("topics")).unwrap();
        std::fs::write(dir.join("topics/a.md"), "一件目").unwrap();

        let first = move_to_trash(&dir, &dir.join("topics/a.md")).unwrap();
        std::fs::write(dir.join("topics/a.md"), "二件目").unwrap();
        let second = move_to_trash(&dir, &dir.join("topics/a.md")).unwrap();

        assert_eq!(std::fs::read_to_string(&first).unwrap(), "一件目");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "二件目");
        assert!(
            load_dir(&dir).notes.is_empty(),
            "捨てたノートが一覧に残っている"
        );
    }

    /// **symlink は辿らないこと。** 2 つの実害を同時に塞ぐ:
    ///
    /// - vault の外を指すリンクを辿ると、外のファイルが一覧に載る。載れば編集でき、
    ///   保存も削除も vault の外で起きる
    /// - 自分自身（や親）を指すリンクは、辿れば再帰が止まらない
    #[test]
    fn walk_does_not_follow_symlinks() {
        let dir = TempDir::new("symlink");
        let outside = TempDir::new("symlink-outside");
        std::fs::write(outside.join("外.md"), "vault の外の秘密").unwrap();
        std::fs::write(dir.join("中.md"), "vault の中").unwrap();

        std::os::unix::fs::symlink(&outside, dir.join("外部フォルダ")).unwrap();
        std::os::unix::fs::symlink(outside.join("外.md"), dir.join("外部ファイル.md")).unwrap();
        // 自分自身を指す循環。辿れば再帰が止まらない。
        std::os::unix::fs::symlink(&dir, dir.join("循環")).unwrap();

        let load = load_dir(&dir);

        let paths: Vec<&Path> = load.notes.iter().map(|n| n.path.as_path()).collect();
        assert_eq!(paths, [dir.join("中.md")], "symlink の先を読んでいる");
        assert_eq!(
            load.failed, 0,
            "仕様どおりの除外を失敗に数えている（毎回警告が出てしまう）"
        );
    }

    /// **読めなかったものが件数と理由で返ること。** 呼び出し元がこれを持たないと、
    /// 権限で欠けた一覧と「もともとその件数しかない vault」を区別できない。
    ///
    /// 読めないファイルは `0000` のディレクトリの中に作って再現する（root で走らせると
    /// 権限を無視して読めてしまうため、**ファイル単体の `0000` では足りない**）。
    #[test]
    fn load_dir_counts_what_it_could_not_read() {

        let dir = TempDir::new("load-failures");
        std::fs::write(dir.join("読める.md"), "# 読める").unwrap();
        let locked = dir.join("鍵付き");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("読めない.md"), "# 読めない").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let load = load_dir(&dir);

        assert_eq!(
            load.notes.len(),
            1,
            "読めるノートまで落としている: {:?}",
            load.notes.iter().map(|n| &n.title).collect::<Vec<_>>()
        );
        assert_eq!(load.failed, 1, "読めなかったのに件数が 0 のまま");
        let reason = load.first_failure.expect("理由が残っていない");
        assert!(
            reason.contains("鍵付き"),
            "どこで失敗したのか分からない: {reason}"
        );

    }

    /// 保存が一時ファイルを残さないこと。残ると vault にゴミが積もり、
    /// 名前が `.md` で終われば一覧にも出る。
    #[test]
    fn save_leaves_no_temporary_file_behind() {
        let dir = TempDir::new("save-tmp");
        let path = dir.join("a.md");
        std::fs::write(&path, "元の中身").unwrap();

        save(&path, "新しい中身").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "新しい中身");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["a.md"], "一時ファイルが残っている");
    }

    /// **保存が元ファイルの権限を引き継ぐこと。** tmp → rename は inode ごと差し替えるので、
    /// 引き継がないと `0600` の非公開ノートが**保存しただけで** `0644` に緩む。
    /// 「打っただけでファイルの公開範囲が変わる」は、保存層が起こしてよい副作用ではない。
    #[test]
    fn save_keeps_the_original_permissions() {

        let dir = TempDir::new("save-perms");
        let path = dir.join("私的なメモ.md");
        std::fs::write(&path, "元の中身").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        save(&path, "新しい中身").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "保存でファイルの権限が緩んだ");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "新しい中身");
    }

    /// **元ファイルが無いときは緩いほうではなく厳しいほう（0600）へ倒すこと。**
    /// 外部で消えたノートを書き戻す場面で umask に任せると、公開範囲が勝手に広がる。
    #[test]
    fn save_creates_a_private_file_when_the_original_is_gone() {

        let dir = TempDir::new("save-perms-missing");
        let path = dir.join("消えたメモ.md");

        save(&path, "書き戻した本文").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "元が無いのに既定の umask で作られた");
    }

    /// **退避は書けない場所を飛ばして次の候補へ落ちること。** 保存が失敗した原因が
    /// ノートのあるディレクトリにあるとは限らないので、退避先を 1 つに賭けない。
    /// 空の `.rescue`（＝予約はできたが書けなかった残骸）を残さないことも見る。
    #[test]
    fn write_rescue_falls_through_to_a_writable_directory() {

        let locked = TempDir::new("rescue-locked");
        let open = TempDir::new("rescue-open");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let path = write_rescue(
            &[locked.to_path_buf(), open.to_path_buf()],
            "メモ.md",
            "失われては困る本文",
        )
        .unwrap();

        assert_eq!(path, open.join("メモ.md.rescue"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "失われては困る本文"
        );
        assert_eq!(
            std::fs::read_dir(&locked).unwrap().count(),
            0,
            "書けなかった場所に残骸ができた"
        );

    }

    /// 退避は既存を絶対に潰さないこと。2 回続けて退避しても 1 回目が残る。
    /// **最後の手段が前回の最後の手段を消したら意味がない。**
    #[test]
    fn write_rescue_never_replaces_an_earlier_rescue() {
        let dir = TempDir::new("rescue-collision");

        let first = write_rescue(&[dir.to_path_buf()], "メモ.md", "1 回目").unwrap();
        let second = write_rescue(&[dir.to_path_buf()], "メモ.md", "2 回目").unwrap();

        assert_ne!(first, second, "同じパスへ 2 回書いている");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "1 回目");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "2 回目");
    }

    /// **予約は `0600` で作られること。** 新規ノートはこの inode に書き続け、`save()` は
    /// 元ファイルの権限を引き継ぐので、ここが umask 任せだと `0644` を永久に引きずる。
    #[test]
    fn reserve_unique_creates_a_private_file() {

        let dir = TempDir::new("reserve-perms");
        let path = reserve_unique(&dir, "新しいメモ.md").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "予約が既定の umask で作られた");
    }

    /// **退避先も `0600` であること。** `.rescue` へ落ちるのは通常経路で保存できなかった本文で、
    /// 元ノートが非公開だった可能性がいちばん高い。最後の手段が公開範囲を広げてはいけない。
    #[test]
    fn write_rescue_creates_a_private_file() {

        let dir = TempDir::new("rescue-perms");
        let path = write_rescue(&[dir.to_path_buf()], "私的なメモ.md", "秘密の本文").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "退避で本文の公開範囲が広がった");
    }

    /// 拡張子なしの名前でも枝番が末尾に付くこと（.trash へ雑ファイルが来ても壊れない）。
    #[test]
    fn reserve_unique_handles_names_without_extension() {
        let dir = TempDir::new("reserve-noext");
        std::fs::write(dir.join("README"), "").unwrap();
        let path = reserve_unique(&dir, "README").unwrap();
        assert_eq!(path, dir.join("README-2"));
    }

    /// ファイルシステムを触らずに 1 件だけパースする。
    fn parsed(raw: &str) -> Note {
        parse_note(
            Path::new("/vault"),
            PathBuf::from("/vault/n.md"),
            raw.to_string(),
            SystemTime::UNIX_EPOCH,
        )
    }

    /// タグの書き方の揺れを吸収すること。
    ///
    /// パーサは YAML ではなく手書きなので（frontmatter が浅いので過剰と判断した）、
    /// **現場にある書き方を回帰テストで固定しないと静かに落ちる。** 落ちたときの症状は
    /// どれも「書いたのに反映されない」で、エラーにはならない。
    #[test]
    fn tags_survive_the_ways_people_actually_write_them() {
        let cases: &[(&str, &[&str])] = &[
            ("tags: [a, b]", &["a", "b"]),
            ("tags: [\"a\", \"b\"]", &["a", "b"]),
            // 片方だけ剥がすと `'a'` という別のタグが残る。
            ("tags: ['a', 'b']", &["a", "b"]),
            // 括弧なし。YAML として合法で、実際に書かれる。
            ("tags: rust", &["rust"]),
            ("tags: rust, iced", &["rust", "iced"]),
            // 行末コメント。
            ("tags: [a, b]  # あとで整理", &["a", "b"]),
            ("tags: rust  # あとで整理", &["rust"]),
            // 値がコメントだけなら、YAML ではタグ無し。
            ("tags:  # あとで書く", &[]),
            ("tags: []", &[]),
            // ブロック形式。インデントの有無は問わない。
            ("tags:\n  - a\n  - b", &["a", "b"]),
            ("tags:\n- a\n- b", &["a", "b"]),
            // 途中の空行で終わらせない。
            ("tags:\n  - a\n\n  - b", &["a", "b"]),
            // 次のキーが来たらそこで終わる（本文の `- ` を拾わないための境界）。
            ("tags:\n  - a\ntitle: メモ", &["a"]),
            ("title: メモ", &[]),
        ];

        for (fm, want) in cases {
            let got = tags(fm);
            let got: Vec<&str> = got.iter().map(String::as_str).collect();
            assert_eq!(got.as_slice(), *want, "frontmatter: {fm:?}");
        }
    }

    /// CRLF のファイルでも frontmatter を読むこと。
    ///
    /// **ここを弾くと巻き添えが大きい。** タグだけでなく `title` も `summary` も落ち、
    /// frontmatter がそのまま本文として画面に出る（`date:` の行がプレビューに出る）。
    #[test]
    fn crlf_frontmatter_is_not_read_as_body() {
        let note = parsed("---\r\ntitle: \"メモ\"\r\ntags: [a, b]\r\nsummary: 要約\r\n---\r\n\r\n本文。\r\n");
        assert_eq!(note.title, "メモ");
        assert_eq!(note.tags, ["a", "b"]);
        assert_eq!(note.preview, "要約");

        // summary が無いときのプレビューは本文の先頭行。行末の `\r` が残らないこと。
        let note = parsed("---\r\ntitle: A\r\n---\r\n\r\n本文の先頭。\r\n");
        assert_eq!(note.preview, "本文の先頭。");
    }

    /// 本文の最初の空でない行をタイトルに使い、その次から preview を取る。
    #[test]
    fn notes_without_metadata_use_the_first_nonempty_line() {
        for first in ["見出し", "# 見出し", "## 見出し"] {
            let note = parsed(&format!("\n \n{first}\n\n本文。\n"));
            assert_eq!(note.title, "見出し");
            assert_eq!(note.preview, "本文。");
            assert!(note.tags.is_empty());
        }
        let note = parsed("ただの本文。\n# 後の見出し\n");
        assert_eq!(note.title, "ただの本文。");
        assert_eq!(note.preview, "# 後の見出し");
    }

    #[test]
    fn empty_notes_fall_back_to_the_filename() {
        for raw in ["", " \r\n\n", "#title: \n#tags:\n"] {
            let note = parsed(raw);
            assert_eq!(note.title, "n");
            assert!(note.preview.is_empty());
            assert!(note.tags.is_empty());
        }
    }

    #[test]
    fn leading_directives_supply_title_and_tags_without_changing_raw() {
        let raw = "\n#tags: 'rust', \"iced\", , 日本語 # 整理, 後日\n\n#title: 明示タイトル\n\n本文。\n";
        let note = parsed(raw);
        assert_eq!(note.title, "明示タイトル");
        assert_eq!(note.tags, ["rust", "iced", "日本語"]);
        assert_eq!(note.preview, "本文。");
        assert_eq!(note.raw, raw);

        let note = parsed("#tags: rust\n# 見出し\n次の行");
        assert_eq!(note.title, "見出し");
        assert_eq!(note.tags, ["rust"]);
        assert_eq!(note.preview, "次の行");

        assert!(parsed("#tags: # 後日, 整理\n本文").tags.is_empty());
        let note = parsed("#title: 最初\n#title: 後\n#tags: rust\n#tags: iced");
        assert_eq!(note.title, "最初");
        assert_eq!(note.tags, ["rust"]);
        assert!(note.preview.is_empty());
    }

    #[test]
    fn directives_stop_at_the_first_ordinary_line() {
        for body in [
            "本文\n#title: 偽\n#tags: rust",
            "```c\n#define X 1\n#title: 偽\n#tags: rust\n```",
            "    #title: コード\n#tags: rust",
            "#unknown: value\n#tags: rust",
        ] {
            assert_eq!(split_directives(body), (None, None, body));
            assert!(parsed(body).tags.is_empty());
        }
    }

    #[test]
    fn frontmatter_precedence_is_per_field() {
        let directives = "#title: 指定\n#tags: rust\n本文\n次の行";
        let note = parsed(&format!(
            "---\ntitle: 既存\ntags: [old]\nsummary: 要約\n---\n{directives}"
        ));
        assert_eq!(note.title, "既存");
        assert_eq!(note.tags, ["old"]);
        assert_eq!(note.preview, "要約");

        let note = parsed(&format!("---\ntitle: 既存\n---\n{directives}"));
        assert_eq!(note.title, "既存");
        assert_eq!(note.tags, ["rust"]);
        assert_eq!(note.preview, "本文");

        for empty_tags in ["tags:", "tags: []", "tags: # 空"] {
            let note = parsed(&format!("---\n{empty_tags}\n---\n{directives}"));
            assert_eq!(note.title, "指定");
            assert!(note.tags.is_empty());
        }

        let note = parsed("---\ntitle: ''\nsummary: 要約\n---\n本文タイトル\n次の行");
        assert_eq!(note.title, "本文タイトル");
        assert_eq!(note.preview, "要約");
    }

    #[test]
    fn crlf_directives_and_body_titles_preserve_byte_offsets() {
        let note = parsed("\r\n#title: 指定\r\n#tags: rust, iced\r\n\r\n本文\r\n");
        assert_eq!(note.title, "指定");
        assert_eq!(note.tags, ["rust", "iced"]);
        assert_eq!(note.preview, "本文");

        let body = "\r\n## 日本語\r\n\r\n次の行\r\n";
        let (title, end) = title_from_body(body).unwrap();
        assert_eq!(title, "日本語");
        assert_eq!(&body[end..], "\r\n次の行\r\n");
        let note = parsed(&format!("#tags: rust\r\n{body}"));
        assert_eq!(note.title, "日本語");
        assert_eq!(note.preview, "次の行");
    }

    #[test]
    fn long_body_titles_are_unicode_safe_and_consume_the_whole_line() {
        let long = "あいうえお".repeat(20);
        for suffix in ["", "\n", "\r\n\r\n次の行"] {
            let note = parsed(&format!("{long}{suffix}"));
            assert_eq!(note.title, "あいうえお".repeat(16));
            assert_eq!(
                note.preview,
                if suffix.contains('次') { "次の行" } else { "" }
            );
        }
        let note = parsed(&format!("#title: {long}\n本文"));
        assert_eq!(note.title, long);
        assert_eq!(note.preview, "本文");
    }

    #[test]
    #[ignore = "実データの vault が要る。VAULT を指定して release で走らせる"]
    fn measure_load_dir_with_real_vault() {
        let root = PathBuf::from(std::env::var_os("VAULT").expect("VAULT を指定する"));
        let warmup = load_dir(&root);
        assert_eq!(warmup.failed, 0);
        assert!(!warmup.notes.is_empty());
        let count = warmup.notes.len();
        drop(warmup);

        for _ in 0..5 {
            let start = std::time::Instant::now();
            let load = load_dir(&root);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            eprintln!("vault load: {} notes / {ms:.2}ms", load.notes.len());
            assert_eq!(load.failed, 0);
            assert_eq!(load.notes.len(), count);
            assert!(
                ms <= 50.0,
                "暖機後の vault 読み込みが 50ms を超えた: {ms:.2}ms"
            );
        }
    }
}
