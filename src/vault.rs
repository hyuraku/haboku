//! 保存層。方針は「`.md` が唯一の真実」（決定: 2026-08-06 / 案2）。
//!
//! SQLite は将来ここに足す全文検索インデックスだが、それは常に再構築可能な
//! キャッシュであって真実ではない。Boostnote が `.cson` を真実にしたせいで
//! 移行コストを払う羽目になった、その轍を踏まないための線引き。
//!
//! STEP 2 の時点では読み込みのみ。書き戻しは STEP 3。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Note {
    pub path: PathBuf,
    pub title: String,
    pub tags: Vec<String>,
    /// vault ルート直下のディレクトリ名（`sources` / `topics` など）。
    /// 実データは tags をほぼ持たず、分類はフォルダが担っていた。
    pub folder: String,
    /// 一覧に出す1行。frontmatter の `summary` があればそれ、無ければ本文の先頭行。
    pub preview: String,
    /// frontmatter を含むファイル全文。エディタにはこれをそのまま渡す。
    pub raw: String,
    /// 最終更新日時。一覧の既定順（新しい順）に使う。
    pub modified: SystemTime,
}

/// `root` 以下の `.md` を再帰的に読む。
///
/// 依存を増やさないため walkdir は使わず std だけで歩く。検証段階では
/// これで十分で、ファイル監視が要るようになった時点で notify に載せ替える。
pub fn load_dir(root: &Path) -> Vec<Note> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    sort_notes(&mut out);
    out
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

fn walk(root: &Path, dir: &Path, out: &mut Vec<Note>) {
    // 読めないものは黙って飛ばさない。無言だと「空 vault」と「権限などで
    // 読めていない」の区別が付かず、原因調査ができなくなる。
    // 通知先は起動時ログ（main の vault 統計と同じ経路）に合わせる。
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("vault: skip dir {} ({e})", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // .git や node_modules に潜っても意味がないので弾く
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            match read_with_mtime(&path) {
                Ok((raw, modified)) => out.push(parse(root, path, raw, modified)),
                Err(e) => eprintln!("vault: skip {} ({e})", path.display()),
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
pub fn save(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, contents)?;
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
pub fn reserve_unique(dir: &Path, file_name: &str) -> std::io::Result<PathBuf> {
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

/// 保存後にメタ情報（タイトル・プレビュー・タグ）を取り直すための公開版。
pub fn parse_note(root: &Path, path: PathBuf, raw: String, modified: SystemTime) -> Note {
    parse(root, path, raw, modified)
}

fn parse(root: &Path, path: PathBuf, raw: String, modified: SystemTime) -> Note {
    let (fm, body) = split_frontmatter(&raw);

    // タイトルの決め方は frontmatter > 最初の見出し > ファイル名 の順。
    // wiki 系のファイルは frontmatter を持ち、雑メモは持たないので両対応が要る。
    let title = fm
        .as_ref()
        .and_then(|f| scalar(f, "title"))
        .or_else(|| {
            body.lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l[2..].trim().to_string())
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let tags = fm.as_ref().map(|f| tags(f)).unwrap_or_default();

    // 実データの vault は tags をほぼ持たず、sources / topics / notes …
    // というルート直下のフォルダが分類を担っていた。サイドバーはそれに合わせる。
    let folder = path
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.parent())
        .and_then(|p| p.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_else(|| "/".to_string());

    // プレビューは frontmatter の summary を最優先。これが無い雑メモは本文の先頭行。
    // raw から取ると frontmatter の `date: "..."` を拾ってしまう（実際に拾っていた）。
    let preview = fm
        .as_ref()
        .and_then(|f| scalar(f, "summary"))
        .unwrap_or_else(|| {
            body.lines()
                .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
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

/// `---` で挟まれた frontmatter を切り出す。無ければ `None`。
fn split_frontmatter(raw: &str) -> (Option<String>, &str) {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return (None, raw);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, raw);
    };
    let fm = rest[..end].to_string();
    let body = rest[end..]
        .trim_start_matches("\n---")
        .trim_start_matches('\n');
    (Some(fm), body)
}

/// `key: value` を1つ拾う。YAML パーサは入れない（frontmatter は浅いので過剰）。
fn scalar(fm: &str, key: &str) -> Option<String> {
    fm.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}:")))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|v| !v.is_empty())
}

/// tags は `tags: [a, b]` と `tags:\n  - a\n  - b` の両方が現場に存在する。
fn tags(fm: &str) -> Vec<String> {
    if let Some(inline) = scalar(fm, "tags")
        && let Some(inner) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
    {
        return inner
            .split(',')
            .map(|t| t.trim().trim_matches('"').to_string())
            .filter(|t| !t.is_empty())
            .collect();
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
                out.push(t.trim().trim_matches('"').to_string());
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn tmp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("haboku-vault-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 空きがあればそのままの名前で予約され、ファイルが実在すること。
    #[test]
    fn reserve_unique_uses_plain_name_when_free() {
        let dir = tmp_dir("reserve-plain");
        let path = reserve_unique(&dir, "a.md").unwrap();
        assert_eq!(path, dir.join("a.md"));
        assert!(path.exists(), "予約したファイルが作られていない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同じ名前を続けて予約すると -2, -3 と枝番が付き、既存を一切潰さないこと。
    /// 秒精度ファイル名の Cmd+N 連打・同名ノートの連続削除がこの性質に乗る。
    #[test]
    fn reserve_unique_never_replaces_existing_files() {
        let dir = tmp_dir("reserve-suffix");
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 移動先が埋まっていても既存を置換せず、枝番を付けて逃がすこと。
    /// **`fs::rename` を直接使うとここで既存が消える**（前身のデータ喪失欠陥の根）。
    #[test]
    fn move_to_never_replaces_an_existing_file() {
        let dir = tmp_dir("move-collision");
        std::fs::write(dir.join("a.md"), "先客").unwrap();
        std::fs::write(dir.join("b.md"), "移動するほう").unwrap();

        let dest = move_to(&dir.join("b.md"), &dir, "a.md").unwrap();

        assert_eq!(dest, dir.join("a-2.md"));
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "先客");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "移動するほう");
        assert!(!dir.join("b.md").exists(), "移動元が残っている");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `.trash` へ退避すると、ファイルは残るが `load_dir` の走査からは外れること。
    /// 同名を 2 回捨てても先に捨てたほうが消えないこと。
    #[test]
    fn move_to_trash_hides_the_note_but_keeps_the_file() {
        let dir = tmp_dir("trash");
        std::fs::create_dir_all(dir.join("topics")).unwrap();
        std::fs::write(dir.join("topics/a.md"), "一件目").unwrap();

        let first = move_to_trash(&dir, &dir.join("topics/a.md")).unwrap();
        std::fs::write(dir.join("topics/a.md"), "二件目").unwrap();
        let second = move_to_trash(&dir, &dir.join("topics/a.md")).unwrap();

        assert_eq!(std::fs::read_to_string(&first).unwrap(), "一件目");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "二件目");
        assert!(load_dir(&dir).is_empty(), "捨てたノートが一覧に残っている");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 拡張子なしの名前でも枝番が末尾に付くこと（.trash へ雑ファイルが来ても壊れない）。
    #[test]
    fn reserve_unique_handles_names_without_extension() {
        let dir = tmp_dir("reserve-noext");
        std::fs::write(dir.join("README"), "").unwrap();
        let path = reserve_unique(&dir, "README").unwrap();
        assert_eq!(path, dir.join("README-2"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
