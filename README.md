<p align="center">
  <img src="assets/icon-1024.png" alt="" width="120">
</p>

<h1 align="center">haboku</h1>

<p align="center">ローカルの Markdown をそのまま扱う、macOS 向けのメモアプリ。</p>

ノートはディスク上の普通の `.md` ファイルとして置かれます。
クラウドにも独自形式にも預けないので、アプリを消してもノートは残ります。

使い方とキーバインドの一覧は [haboku のページ](https://hyuraku.github.io/haboku/) にあります。

現在はソースからビルドして使う段階です。
配布に必要な Developer ID 署名と notarization はまだ行っていません。

## インストール

macOS と、stable の Rust ツールチェイン（edition 2024）が要ります。

`.app` として組み立てる場合は次の手順になります。

```sh
git clone https://github.com/hyuraku/haboku.git
cd haboku
sh dist/build-app.sh
cp -R target/dist/haboku.app /Applications/
```

生成先は `target/dist/haboku.app` です。
`dist/build-app.sh` が行うのは ad-hoc 署名までなので、ビルドした Mac での利用を前提とします。

Cargo からそのまま起動することもできます。

```sh
cargo run --release
```

初回の起動でノートの置き場所を選びます。
既定の候補は `~/Documents/haboku` で、すでにある Markdown のフォルダを選んだ場合は、それをそのまま vault として開きます。

## ライセンス

[MIT](LICENSE)
