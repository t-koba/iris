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

| OS | 状態 |
|---|---|
| macOS 14 以降 | 標準ビルド |
| Linux（Wayland / X11） | 標準ビルド |
| Windows | `minimal` ビルド（動画・PDF プレビューなし） |

ビルドには `rust-toolchain.toml` で指定した Rust（1.97.1）を使います。

標準ビルドには、次のライブラリも必要です。

- **FFmpeg の開発用ライブラリと `pkg-config`**（動画サムネイル用）
  - macOS: `brew install ffmpeg pkg-config`
  - Ubuntu: `sudo apt-get install pkg-config clang libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libavdevice-dev libswscale-dev libswresample-dev`
- **pdfium の共有ライブラリ**（macOS 以外で PDF をプレビューする場合のみ。実行時に読み込みます）。
  実行ファイルと同じフォルダ、作業ディレクトリ、システムのライブラリパスの順に探し、
  見つからなければ PDF のサムネイルだけを省きます

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
- `release.yml`: `v*` タグを push すると、3 つの OS のパッケージを Release に添付します
- `fuzz-long.yml`: 週に 1 回、長時間の fuzz を実行します
