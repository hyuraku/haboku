//! 既定メニューの Quit 項目を、保存ガードの通る経路へ付け替える（ADR-0021）。
//!
//! **このファイルは Cmd+Q でノートが消える穴を塞ぐためだけにある。**
//!
//! winit 0.30.13 は macOS の既定メニューを自前で組み立て、Quit 項目に
//! `terminate:` セレクタと key equivalent `"q"` を刺す
//! （`winit-0.30.13/src/platform_impl/macos/menu.rs:66-73`）。`terminate:` は
//! NSApp を直接終了させ、winit は差し止められる `applicationShouldTerminate:` を
//! 実装していないので、**`Cmd+Q` は `WindowEvent::CloseRequested` を一度も出さずに
//! プロセスを殺す**（実機で確認: デバウンス内の編集がノート 0 bytes のまま消えた）。
//!
//! 対して ✕ ボタンは `performClose:` → `windowShouldClose:` → `CloseRequested` と流れ、
//! `close_window` の保存ガードに乗る。**だから Quit 項目の飛び先をそちらへ揃える。**
//! `setTarget(nil)` にした action はレスポンダチェーン（= キーウィンドウ）へ配送されるので、
//! `Cmd+Q` もメニューの「Quit haboku」クリックも ✕ と同一経路になる。
//! 保存が済めば窓が閉じ、iced は最後の窓が閉じた時点で終了する。
//!
//! メニューを作り直さないのは、About / Services / Hide を組み直す手間だけ増えて
//! 得るものが無いため（却下案の全文は ADR-0021）。

use objc2::MainThreadMarker;
use objc2::sel;
use objc2_app_kit::NSApplication;

/// Quit 項目の action を `terminate:` → `performClose:` へ付け替える。
///
/// **起動後に 1 回だけ呼ぶ。** winit はメニューを最初のイベント配送より前
/// （`applicationDidFinishLaunching`）に取り付けるので、`update()` に届く頃には必ず在る。
///
/// `Err` は「今日と同じ挙動に留まる」ことを意味する（悪化はしない）。呼び出し側は
/// 黙って捨てず常駐エラー行に出す — `Cmd+Q` が守られていないことを知らせるため。
pub fn retarget_quit_to_close() -> Result<(), String> {
    // AppKit をメインスレッド以外から触ると未定義動作。iced の `update()` は
    // メインスレッドで走るが、それを型で確かめてから進む。
    let Some(mtm) = MainThreadMarker::new() else {
        return Err("メインスレッドではありません".to_string());
    };

    let Some(menubar) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return Err("メニューバーがありません".to_string());
    };

    // アプリメニューの位置を決め打ちにせず、`terminate:` を送る項目を名前ではなく
    // **action で**探す。項目名はプロセス名から組み立てられる（"Quit haboku"）ので、
    // 名前で当てるとバンドル名の変更で静かに外れる。
    let mut patched = 0;
    for item in menubar.itemArray().iter() {
        let Some(submenu) = item.submenu() else {
            continue;
        };
        for entry in submenu.itemArray().iter() {
            if entry.action() != Some(sel!(terminate:)) {
                continue;
            }
            // SAFETY: `performClose:` は NSWindow が実装する既存のセレクタで、
            // nil ターゲットの action はレスポンダチェーンへ配送される。
            // 受け手が居なければメニュー検証が項目を無効化するだけで、誤配送は起きない。
            unsafe {
                entry.setAction(Some(sel!(performClose:)));
                entry.setTarget(None);
            }
            patched += 1;
        }
    }

    if patched == 0 {
        // winit が既定メニューを組まなくなった場合はここに来る。**黙って成功にしない。**
        return Err("Quit 項目が見つかりません".to_string());
    }
    Ok(())
}
