//! テスト用の使い捨てディレクトリ。
//!
//! **`#[cfg(test)]` にできない。** `main.rs` はバイナリクレートなので、lib 側の
//! `#[cfg(test)]` モジュールは見えない。3 ファイル（`main` / `vault` / `config`）で
//! 同じフィクスチャを共有する唯一の経路がここ。`publish = false` のクレートなので
//! 公開 API になること自体のコストは無い。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// 使い捨てディレクトリ。**落ちても片付く。**
///
/// 後始末を関数末尾に手書きすると、アサートが落ちた瞬間に片付けが走らない。
/// 権限を落とすテスト（`0o000` / `0o555`）ではそれが temp に残り、次の実行を汚す。
/// `Drop` に置くと、パニックで巻き戻るときも必ず通る。
pub struct TempDir(PathBuf);

impl TempDir {
    /// `name` ごとに別のディレクトリを作る。前回の残骸があれば消してから作る。
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("haboku-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

/// `Deref` だけでは `fs::set_permissions(&dir, ..)` のような
/// `impl AsRef<Path>` を取る API に渡せない（自動 deref は型引数の解決に効かない）。
impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        unlock(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 権限を落としたディレクトリを開けて回る。**先に chmod しないと読めない**ので、
/// 降りる前に自分を開ける。ファイルは親が書ければ消せるのでディレクトリだけでよい。
fn unlock(dir: &Path) {
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            unlock(&entry.path());
        }
    }
}
