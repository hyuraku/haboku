//! 選んだ vault の場所を覚えておく。ADR-0015。
//!
//! 中身は**パス 1 行だけ**。JSON も TOML も入れない（保存する値が 1 つしかない）。
//!
//! **`$HOME` はここでは読まない。** 呼ぶ側から `home` を渡してもらうことで、
//! テストが環境変数を書き換えずに済む（環境変数はプロセス全体で共有なので、
//! 並列に走るテストから触ると互いを壊す）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 記憶したパスを書くファイル。`$HOME/Library/Application Support/haboku/vault`
///
/// macOS がアプリの設定を置く場所。`.app` になっても書ける（サンドボックス外なので
/// パスは変わらない）。
pub fn config_file(home: &Path) -> PathBuf {
    home.join("Library/Application Support/haboku/vault")
}

/// 初回セットアップで提示する既定の候補。`$HOME/Documents/haboku`
///
/// **ここを開くのはユーザーが選んだあとだけ。** TCC が `~/Documents` を守っているので、
/// アプリが自分から覗くと許可ダイアログが出る（ADR-0015）。
pub fn default_vault(home: &Path) -> PathBuf {
    home.join("Documents/haboku")
}

/// 記憶したパスを読む。**その場所が今も在るかは確かめない**（判断は呼ぶ側）。
///
/// 読めない・空・空白だけ → `None`。壊れた設定を「無いもの」として扱えば、
/// 呼ぶ側はセットアップへ倒す 1 本道でよくなる。
pub fn read_vault(config: &Path) -> Option<PathBuf> {
    let raw = fs::read_to_string(config).ok()?;
    let line = raw.trim();
    if line.is_empty() {
        return None;
    }
    Some(PathBuf::from(line))
}

/// 選んだパスを記憶する。親ディレクトリが無ければ作る。
///
/// **失敗を握り潰さない。** ここで失敗すると次回起動でまたセットアップ画面が出るので、
/// 呼ぶ側が画面に出せるよう `io::Result` を返す。
pub fn write_vault(config: &Path, root: &Path) -> io::Result<()> {
    if let Some(parent) = config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config, format!("{}\n", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 使い捨ての `$HOME` を作る。テストごとに別のディレクトリを持つ。
    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "haboku-config-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn config_lives_under_application_support() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            config_file(home),
            PathBuf::from("/Users/someone/Library/Application Support/haboku/vault")
        );
    }

    #[test]
    fn the_default_candidate_is_documents_haboku() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            default_vault(home),
            PathBuf::from("/Users/someone/Documents/haboku")
        );
    }

    #[test]
    fn a_written_path_reads_back() {
        let home = temp_home("roundtrip");
        let config = config_file(&home);
        let root = home.join("Documents/haboku");

        write_vault(&config, &root).unwrap();

        assert_eq!(read_vault(&config), Some(root));
    }

    #[test]
    fn a_missing_config_reads_as_none() {
        let home = temp_home("missing");
        assert_eq!(read_vault(&config_file(&home)), None);
    }

    /// 空ファイルや改行だけのファイルを `Some(PathBuf::from(""))` にすると、
    /// 呼ぶ側が「記憶がある」と誤解してルート直下を開きにいく。
    #[test]
    fn an_empty_config_reads_as_none() {
        let home = temp_home("empty");
        let config = config_file(&home);
        fs::create_dir_all(config.parent().unwrap()).unwrap();

        for contents in ["", "\n", "   \n"] {
            fs::write(&config, contents).unwrap();
            assert_eq!(read_vault(&config), None, "contents={contents:?}");
        }
    }

    /// 日本語やスペースを含むパスをそのまま往復できること。
    /// フォルダ選択で選ばれるのは大抵ユーザーが名付けたフォルダなので、ここは通らないと困る。
    #[test]
    fn paths_with_japanese_and_spaces_survive_the_round_trip() {
        let home = temp_home("unicode");
        let config = config_file(&home);
        let root = home.join("Documents/わたしの メモ帳");

        write_vault(&config, &root).unwrap();

        assert_eq!(read_vault(&config), Some(root));
    }
}
