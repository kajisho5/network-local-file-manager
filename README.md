# network-local-file-manager

同じLAN上にあるPC同士で、監視フォルダのファイル変更(作成・更新・削除)を即座に検知し、OSネイティブのトースト通知でお知らせする常駐アプリです。サーバーPCを常時起動しておく必要のないP2P方式で、mDNSにより同じネットワーク上の他のエージェントを自動的に見つけます。

現在のスコープは **検知+通知のみ(MVP)** です。ファイルの実同期(コピー・コンフリクト解決)はまだ実装していません。

## アーキテクチャ

```
core/       Rust: ファイル監視・mDNS探索・変更通知プロトコルなど、GUIに依存しないロジック
src-tauri/  Rust + Tauri: トレイ常駐アプリ本体(ファイル監視の起動、通知表示、設定コマンド)
src/        フロントエンド: 監視フォルダ・検出ピアを表示する設定画面(素のHTML/JS)
```

- **ファイル監視**: [`notify`](https://docs.rs/notify) crate(Windowsは`ReadDirectoryChangesW`、macOSは`FSEvents`)。連続書き込みを1つの変更にまとめるデバウンス処理付き。
- **ピア探索**: [`mdns-sd`](https://docs.rs/mdns-sd) crateで `_lfsync._tcp.local.` を名乗り、LAN上の他エージェントを自動発見。中央サーバー不要。
- **変更通知の伝搬**: 変更を検知したエージェントが、発見済みの各ピアへTCP(既定ポート `47821`)でJSONメッセージを送信。受信側がOSネイティブのトースト通知を表示。
- **設定の永続化**: 監視フォルダの一覧とこのPCのpeer IDを、OSの設定ディレクトリ配下の `network-local-file-manager/config.json` に保存。

## 必要環境

- [Rust](https://www.rust-lang.org/tools/install)(stable)
- [Node.js](https://nodejs.org/)(npm) — Tauri CLIの実行用
- OSごとのWebView依存ライブラリ:
  - **Windows**: 追加インストール不要(WebView2は多くの環境に標準搭載。無ければ[Evergreen Bootstrapper](https://developer.microsoft.com/microsoft-edge/webview2/)を導入)
  - **macOS**: 追加インストール不要(WKWebViewはOS標準)
  - **Linux**(開発機として使う場合): `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` など。詳細は[Tauriの公式セットアップ手順](https://tauri.app/start/prerequisites/)を参照

このリポジトリの `core` クレートはGUI/WebView依存がないため、上記が無い環境でも `cargo test -p lfsync-core` は実行できます。

## セットアップ

```sh
npm install
```

## 開発モードで起動

```sh
npm run tauri dev
```

トレイアイコンにアプリが常駐し、ウィンドウを閉じてもバックグラウンドで動き続けます(トレイメニューの「終了」で完全終了)。

## アイコンについて

`src-tauri/icons/` には仮のプレースホルダーアイコンのみ入っています。配布用ビルド(インストーラー作成)には `.ico`(Windows)・`.icns`(macOS)が必要なので、実際のアイコン画像を用意したら以下で一式生成してください。

```sh
npm run tauri icon path/to/your-icon.png
```

## 配布用ビルド

```sh
npm run tauri build
```

各OS上でそのOS向けのインストーラー(`.msi`/`.exe`、`.dmg`/`.app`)が生成されます。クロスコンパイルは基本的にサポートされていないため、Windows向けはWindows上で、macOS向けはmacOS上でビルドしてください。

## テスト

```sh
# GUIに依存しないコアロジックのテスト(どの環境でも実行可能)
cargo test -p lfsync-core

# アプリ全体のビルド確認(WebView依存ライブラリが必要)
cargo build -p network-local-file-manager
```

## 現在の制限事項 / 今後

- **ファイル実同期は未実装**: 現状は変更の検知と通知のみ。実際にファイルをコピーして反映する機能は次のステップです。
- **リネームの検出は簡易的**: OSやファイルシステムによってはリネームが「変更」として通知される場合があります。
- **同一LAN内が前提**: mDNS(マルチキャスト)を使うため、ルーターをまたいだ別ネットワークのPCは自動検出できません。
- **暗号化・認証なし**: 変更通知は平文TCPで送信されます。信頼できるLAN内での利用を想定しています。
