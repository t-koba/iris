# Iris

Rust と [iced](https://iced.rs) で作ったクロスプラットフォームのファイルマネージャーです。
ローカルディスク・NFS/SMB 共有・S3・pCloud を同じ画面で扱います。
写真・RAW・動画・PDF の一覧表示とプレビューを特に速くすることを目標にしています。

## 主な機能

- **閲覧**: 2 ペイン、タブ、リスト／グリッド表示、10 万件規模の仮想スクロール、パンくず・パス入力補完
- **接続先**: ローカル、NFS マウント、SMB2/3（OS のマウントを使わない独自クライアント）、S3、pCloud。
  Linux では GIO（`gio mount`・デスクトップのファイルマネージャー）でマウントした共有もボリュームに表示します
- **高速転送**: マウント済みの SMB/NFS 共有を SSH ホストに対応付けると、SFTP / rsync で直接転送します
- **プレビュー**: JPEG・PNG・GIF・BMP・TIFF・WebP・ICO・AVIF・JPEG 2000・JPEG XR・JPEG XL・HEIC/HEIF、
  RAW の埋め込みプレビュー（ARW・CR3・DNG・NEF。`raw-full` では CR2・ORF・RAF・RW2・X3F も）、動画（FFmpeg）、PDF
- **フィルタ**: 名前のあいまい一致、種類、サイズ、日付、カメラ・レンズ、GPS 範囲（地図）、PDF の本文、サブフォルダの再帰検索
- **ファイル操作**: コピー・移動（移動は「コピー → 検証 → 元を削除」の順）、一時停止・再開・キャンセル、
  アプリ再起動後の再開、競合時の確認、元に戻す／やり直し。
  コピー先の更新日時は元のファイルと同じになり（S3・pCloud など保持できない保存先を除く）、
  シンボリックリンクはリンクのまま複製します（リンクを作れない保存先ではリンク先の内容をコピーし、
  親フォルダを指すリンクはたどりません）
- **その他**: コマンドパレット、ショートカットの変更、日本語／英語、ライト／ダーク

## 動作環境

| OS | リリース版 |
|---|---|
| macOS 14 以降 | 標準プレビュー機能を含む |
| Linux（Wayland / X11） | 標準 feature でビルド。配布パッケージには検証済み PDFium を同梱 |
| Windows | `minimal` 構成。動画・PDF は無効で、RAW は ARW・CR3・DNG・NEF のみ |

ソースビルドに使う Rust は `rust-toolchain.toml` で指定しています（1.97.1）。

### ソースから標準ビルドする場合

標準 feature は `video`・`pdf`・`raw-full` です。動画プレビューには FFmpeg の開発用ライブラリと `pkg-config` が必要です。

- macOS: `brew install ffmpeg pkg-config`
- Ubuntu: `sudo apt-get install pkg-config clang libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev`

macOS 以外で PDF をプレビューするには、実行時に PDFium 共有ライブラリが必要です。Iris は実行ファイルの隣、インストール先の `lib/iris`（例: `/usr/lib/iris`）、作業ディレクトリ、システムのライブラリパスの順に探し、見つからない場合は PDF サムネイルを表示しません。

## リリースのダウンロードとインストール

[GitHub Releases](https://github.com/t-koba/iris/releases) から利用する OS のファイルをダウンロードしてください。`v<version>` タグはルート `Cargo.toml` の workspace version と一致し、`main` 上のコミットを指す必要があります。公開前に全 OS の成果物と SHA-256 チェックサムを検証するため、一部の OS だけの不完全なリリースは公開されません。各成果物には同名の `.sha256` ファイルが付属します。

### macOS

- `Iris-macos.zip`: 展開して `Iris.app` を `/Applications` に移動します。
- `Iris-macos.pkg`: インストーラを実行すると `Iris.app` が `/Applications` にインストールされます。

macOS 14 以降が必要です。コード署名 ID が設定されていないビルドではアプリは ad-hoc 署名になり、公証は notary profile が設定された場合に限られます。`.pkg` インストーラ自体は署名・公証されません。Gatekeeper の警告が出ないことは保証されないため、配布時の署名状況を確認してください。

### Linux

- `iris_<version>_<arch>.deb`: Debian/Ubuntu 系では、ダウンロードしたファイルを指定してインストールします。

  ```bash
  sudo apt install ./iris_<version>_<arch>.deb
  ```

  削除する場合は `sudo apt remove iris` を実行します。`.deb` はリンクされた共有ライブラリの依存関係を宣言します。
- `Iris-linux-appdir.tar.gz`: 展開後、`Iris.AppDir/AppRun` を実行します。これは AppDir のアーカイブであり、AppImage ではありません。ホストに必要な共有ライブラリが必要です。

Linux のリリース成果物には、検証済みの PDFium 共有ライブラリ（PDFium 159.0.8138.0）とライセンスが同梱されます。AppDir では `Iris.AppDir/usr/lib/iris/libpdfium.so`、`.deb` では `/usr/lib/iris/libpdfium.so` に配置され、Iris は実行ファイルの隣、インストール先の `lib/iris`、作業ディレクトリ、システムのライブラリパスの順に探します。

### Windows

- `Iris-windows.zip`: 展開して `Iris.exe` を実行します。
- `Iris-windows-setup.exe`: 管理者権限なしのユーザー単位で `%LOCALAPPDATA%\Programs\Iris` にインストールします。スタートメニューに登録され、インストーラでデスクトップショートカットも選べます。アンインストールは Windows のアプリ一覧から行えます。

現在の Windows リリースは `minimal` ビルドです。動画・PDF プレビューと `raw-full` の RAW 形式は含まれません。また、現在の workflow では実行ファイルやインストーラにコード署名を行わないため、Windows の警告や SmartScreen の表示は環境によって異なります。

### チェックサムの確認

インストール前に成果物と同名の `.sha256` ファイルを使って SHA-256 を確認してください。Linux では `sha256sum -c Iris-linux-appdir.tar.gz.sha256`、macOS では `shasum -a 256 -c Iris-macos.zip.sha256` を使えます。Windows PowerShell では `Get-FileHash -Algorithm SHA256 .\Iris-windows.zip` の値を `Iris-windows.zip.sha256` の先頭のハッシュと照合してください。

## ビルドと実行

フォントやアイコンは実行ファイルに埋め込まれています。

```bash
cargo run -p iris-ui-shell
```

フォルダを引数に渡すと、そのフォルダを開いた新しいウィンドウで起動します（「新しいウィンドウで開く」もこの方法です）。

```bash
cargo run -p iris-ui-shell -- ~/Pictures
```

リリースビルドの実行ファイルは `target/release/Iris` に出力されます。

```bash
cargo build --release -p iris-ui-shell
```

FFmpeg や pdfium を使わない最小構成でもビルドできます。

```bash
cargo build --release -p iris-ui-shell --no-default-features --features minimal
```

| フィーチャー | 内容 |
|---|---|
| `video` | FFmpeg による動画サムネイル（標準で有効） |
| `pdf` | pdfium による PDF プレビュー（標準で有効） |
| `raw-full` | すべての RAW 形式（標準で有効） |
| `raw-core` / `minimal` | RAW は ARW・CR3・DNG・NEF のみ。`minimal` は動画・PDF も無効 |

## 保存先

| 内容 | 場所 |
|---|---|
| 設定（`settings.json`・`layout.json`・`connections.json`） | OS の設定ディレクトリ（例: macOS は `~/Library/Application Support/iris`） |
| キャッシュ索引・転送ジョブ（SQLite） | OS のローカルデータディレクトリの `iris/` |
| ログ | OS のデータディレクトリの `iris/logs/` |
| ヘルプ（この README を書き出したもの） | OS のローカルデータディレクトリの `iris/help/` |
| パスワード・トークン | OS の資格情報ストア（キーチェーンなど） |

設定ファイルや DB の版が現在の版と違う場合、古いデータは読み込みません。
設定ファイルは `.corrupt-<時刻>` を付けて退避したうえで初期値で起動し（起動時に通知します）、
キャッシュ索引と中断中のジョブは作り直します。
設定とタブはウィンドウを閉じるときにも保存します。

### PDF 本文検索の上限

アプリを閉じてから `settings.json` の `pdf_text_search` を編集できます。
未指定の項目は次の既定値になり、既存の設定ファイルもそのまま読み込めます。

```json
"pdf_text_search": {
  "input_bytes": 8388608,
  "decoded_bytes": 16777216,
  "text_bytes": 1048576
}
```

各 PDF について、実際に読み込む入力は 8 MiB、全ストリームの展開後データの合計は
16 MiB、空白を正規化した抽出本文は UTF-8 で 1 MiB までです。通常の本文検索を保ちつつ、
ファイルや圧縮データによるメモリ消費を制限する既定値です。ワーカー数は再帰検索ごとに
最大 4 並列で、複数の検索を同時に実行すると、その分だけ並列数が増えます。
入力バッファに加えて、展開データは一度に 1 ストリームだけ保持します。
文字列解析の作業領域やアロケータの容量も必要なため、これらはプロセス全体のメモリ上限ではありません。
値を増やす場合は並列数も考慮してください。`0` は無制限ではなく、その予算での処理停止です。

上限・読み取りエラー・未対応/壊れたストリームで処理が止まった PDF は検索候補から除外せず、
注釈に「PDF 本文の走査は不完全です」（英語では `PDF text scan incomplete`）を表示します。抽出済みの部分に一致があればその抜粋を表示し、
見つからない場合は一致の有無が不明であると表示します。入力と展開の予算をちょうど使い切った場合も、
追加の読み取りで上限を超えないよう保守的に不完全扱いにします。

本文検索は簡易ストリーム解析で、ページ選択やページ走査数は提供しません（API の
`pages_scanned` は常に `None`）。暗号化・複雑なフォント/文字マッピング・未対応のフィルタ等を
完全に解釈するものではなく、OCR も行いません。上限に達しなかった結果も本文の完全な抽出を保証しません。
これらの設定は PDF プレビューやサムネイル描画には影響しません。

## 開発

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

| 内容 | コマンド |
|---|---|
| fuzz（nightly と `cargo-fuzz` が必要） | `FUZZ_RUNS=1000 ./scripts/fuzz-all.sh` |
| SFTP / SMB の結合テスト（Docker が必要） | `./scripts/perf-transfer-containers.sh` |
| 依存クレートの脆弱性監査（`cargo-audit`） | `./scripts/audit-dependencies.sh` |
| パッケージ作成 | `./scripts/package-macos-app.sh`、`./scripts/package-linux-appdir.sh`、`./scripts/package-windows.ps1` |

GitHub Actions の構成は次のとおりです。

- `ci.yml`: push・プルリクエストのたびと週に 1 回、整形・lint・テスト・ビルドを実行します
- `release.yml`: `main` 上の version 一致 `v<version>` タグから、3 つの OS のアーカイブ・インストーラと SHA-256 チェックサムを検証して GitHub Release に添付します
- `fuzz-long.yml`: 週に 1 回、長時間の fuzz を実行します
