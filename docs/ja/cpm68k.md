[English](../cpm68k.md) | 日本語

# CP/M-68KとBDOSモニタ

CP/M-68K 1.3を、Tiny68K互換の機械（[68000 CPUコア](m68k.md)）と自作のBIOSで動かし、ICEでBDOS・BIOSの呼び出しを観察します。

```sh
sh examples/cpm68k.sh            # コンソールはこの端末（^Cで終了）
sh examples/cpm68k.sh --ice      # ICE: http://127.0.0.1:6809/（--dap でVS Codeなどからも）
```

初回は `scripts/fetch-cpm68k.py` で配布物を取得し、`scripts/cpm68k-disk.py` でディスク `artifacts/cpm68k/a.img` を作ります。
ディスクへの書き込みは残ります（作り直すにはファイルを消してください）。

```text
A>DIR *.SUB
A: RELOC1   SUB : RELOC2   SUB : CLINK    SUB : CLINKE   SUB : CLINKF   SUB
...
A>AS68 -I AS68INIT          ← 初回だけ。アセンブラの記号表を作る
A>SUBMIT C HELLO            ← CP68 → C068 → C168 → AS68
A>SUBMIT CLINK HELLO        ← LO68 でリンクして HELLO.68K
A>HELLO
Hello from Alcyon C on CP/M-68K
1+2+...+10 = 55
```

## 配布物と構成

| もの | 出どころ | リポジトリ |
|---|---|---|
| CP/M-68K 1.3（`68kv1_3.zip`、SHA-256で確認） | [The Unofficial CP/M Web Site](http://www.cpm.z80.de/)、[ライセンス](http://www.cpm.z80.de/license.html) | 含めない（取得スクリプト） |
| システム本体 `CPM15000.SR`（BIOSなし、`$15000`〜`$1ACC1`） | 配布物の `DISK8` | 含めない |
| BIOS `examples/cpm68k/bios.s` と組み立てた `bios.sr`・`bios.lst`・`bios.map` | 自作 | 含める |
| ディスクイメージ | `scripts/cpm68k-disk.py` で作る | 含めない |

Alcyon CがCP/Mのライセンスに含まれるかは明記がないため、本体と同じく取得スクリプトで扱います。

メモリの配置：

| 番地 | 内容 |
|---|---|
| `$000000`〜`$0003FF` | 例外ベクタ（TRAP #2がBDOS、TRAP #3がBIOS） |
| `$015000`〜`$017321` | CCP（`$15000` が入口。`_ccp`＝`$150BC` がウォームブート先） |
| `$017322`〜`$01AFFF` | BDOS（TRAP #2の入口 `_traphnd` から） |
| `$01B000`〜 | BIOS（`_init`） |
| `$020000`〜`$FDFFFF` | TPA（プログラムはここに再配置して読み込まれる） |

起動はモニタROMなしで、エミュレータが2つのS-recordを読み込み、`$15000` からスーパーバイザ状態で始めます。
システムはスタックを設定して `JSR $1B000`（BIOSの `_init`）を呼び、BDOSを初期化してCCPのプロンプトを出します。

## BIOS

`examples/cpm68k/bios.s`（GNU as、Motorola記法）。組み立ては `python3 scripts/build-cpm68k-bios.py`
（Ubuntuの `binutils-m68k-linux-gnu`。組み立てた結果をリポジトリに入れているので、動かすだけなら不要）。

- `_init`：TRAP #3のベクタを設定し、DUARTのチャネルAを初期化して、D0＝0（ドライブA:、ユーザー0）で戻ります。
- TRAP #3：D0.Wの機能番号（0〜22）で表を引きます。D1〜D7・A0〜A6は保存します。
- コンソールはDUARTのチャネルA、LIST・PUNCH・READERはチャネルBです。
  `CONIN` は空回りせず、受信割り込み（レベル2、ベクタ64）を有効にして `STOP` で待ちます。割り込み処理はIMRを0に戻すだけです
  （要求は1バイト読むまで出続けるため）。
- ディスクA:はIDEです。BDOSは128バイトのセクタで読み書きするので、BIOSが512バイトのIDEセクタを1つ持ち、
  読み込みはそこから写し、書き込みはそこを書き換えてすぐIDEへ書きます（ライトスルー）。IDEの語はバイトを入れ替えて読み書きします。
- `GETMRT` はTPAを `$20000`〜`$FE0000` とする表を返します。`SETEXC` はTRAP #3以外のベクタを書き換えます。

## ディスクの形式

8 MiB（512バイト×16384）。1トラック＝128バイト×32、2048トラック、予約トラック1、ブロック4 KiB（2047個）、ディレクトリ512項目（ブロック0〜3）、
ディレクトリの割り当て表は16ビットのブロック番号です。番号はリトルエンディアンで、8080版のCP/Mと同じ並びです（BDOSは68000で動くのにこの並び）。

```sh
python3 scripts/cpm68k-disk.py dist a.img [FILE[=NAME]]...   # 配布物の道具一式＋ファイル
python3 scripts/cpm68k-disk.py create a.img FILE...          # 空のディスク＋ファイル
python3 scripts/cpm68k-disk.py add a.img FILE...
python3 scripts/cpm68k-disk.py ls a.img
python3 scripts/cpm68k-disk.py get a.img X.SUB -
```

配布物のコマンドは `.REL`（再配置情報付き）ですが、BDOSのプログラムロード（関数59）が再配置するので、`.68K` の名前で書き込み、RELOCは使いません。
テキストファイル（`.C`・`.H`・`.SUB` など）は改行をCR LFにして、末尾を^Zで埋めます。

## BDOSモニタ（`src/ice/cpm68k.rs`）

ICEのOSの層（`OsMonitor`）のCP/M-68K版です。システムの先頭の `CP/M-68K` の文字列と、TRAP #2がBDOSを指すことで見つけます。

| 項目 | 内容 |
|---|---|
| 捕捉 | `TRAP #2`（BDOS、機能番号D0.W、引数D1）と `TRAP #3`（BIOS、D0.W・D1・D2）。機能番号は実行直前のレジスタから決めます（`svc_regs`） |
| 名前 | BDOSは `F_OPEN`・`C_WRITESTR`・`P_LOAD` などCP/Mで一般的な名前、BIOSは `BIOS_CONOUT` など。日本語の要約と説明付き |
| 内部の呼び出し | BIOSの呼び出しと、BDOSの中からのBDOS呼び出し（`P_LOAD` の中の `F_READ` など）は内部呼び出しとして、元のBDOS呼び出しの下に数えます |
| 引数・結果 | 文字、`$` で終わる文字列、FCBのファイル名（`A:HELLO.C`）、ランダムレコード番号、DMA番地、ロードパラメータブロック（TPA、ベースページ、テキスト・データ・BSS） |
| エラー | ディレクトリ操作の `$FF`、順書き・ランダム読み書き・プログラムロードの失敗、BIOSの読み書きの失敗。`F_SFIRST`／`F_SNEXT` の `$FF`（項目の終わり）はエラーにしません |
| 状態 | BDOSの内部の配置は公開されていないため、現在のドライブ・ユーザー番号・DMA番地・開いているFCB・TPA・読み込んだプログラムを、呼び出しの引数と結果から学びます |
| プログラム | CCPを1、`P_LOAD` で読み込んだプログラムを2以降として表示し、`P_TERMCPM`・ウォームブートでCCPに戻ります |
| メモリ | 例外ベクタ・CCP・BDOS・BIOS・TPA・プログラム・ベースページの領域 |

OSタブでは、システム情報（実行中のプログラム、ドライブ・ユーザー、DMA、TPA、開いているファイル）、プログラム表、メモリ上のプログラムとシステム、
メモリ領域、BDOS・BIOSの呼び出しの記録（呼び出しを選ぶと、レジスタD0〜D2・A0・A1・A7・SRの入力と出力、内部の呼び出しの木）を表示します。

```sh
# 名前で停止（HTTP API）
curl -X POST 'localhost:6809/api/os/break?code=F_OPEN'                 # 呼び出し前
curl -X POST 'localhost:6809/api/os/break?code=P_LOAD&when=return'     # 復帰後
curl -X POST 'localhost:6809/api/os/break?code=BIOS_READ'              # BIOSも名前か「BIOS 13」で
curl -X POST 'localhost:6809/api/os/break?errors=1'                    # エラー復帰
```

DAPでは、例外ブレークポイントの「システムコールの呼び出し」「システムコールの復帰」の条件に名前をカンマ区切りで書きます（[ICE](ice.md)）。

## シンボルとソース

`examples/cpm68k.sh` は、BIOSのリスト（`bios.lst`＋`bios.map`）とシステムの番地表（`CPM15000.MAP`）を読み込みます。
逆アセンブルとスタックにCCP・BDOSの関数名（`_traphnd`、`_pgmld` など、NM68の8文字）とBIOSのラベルが付き、
BIOSはソース行でブレーク・ステップできます（VS Codeでは `bios.s` に行ブレークポイントを置けます）。

## 検証

```sh
python3 scripts/fetch-cpm68k.py
cargo test --release --test cpm68k -- --ignored
cargo test --release --lib tiny68k            # DUARTとIDE
```

- 起動して `DIR`・`TYPE`・`STAT`・`PIP X.SUB=C.SUB` の出力を確かめ、その裏の `F_OPEN`・`F_MAKE`（一時名 `X.$$$`）・`F_RENAME`・`P_LOAD`・`F_READ` と、
  内部のBIOS呼び出しの記録、学んだドライブとTPAを確かめます。
- `F_OPEN` の呼び出し前（D0＝15、D1がCCPのFCB `STAT    68K`）、`P_LOAD` の復帰後（プログラム表のベースページ `$020000`、イメージ `STAT`）、
  `TYPE NOSUCH.TXT` の `F_OPEN` のエラー復帰で停止します。
- Alcyon Cで `examples/cpm68k/hello.c` をコンパイル・リンクして実行します。
- DAP：`F_SFIRST` の呼び出しで止め、スレッド名に `CP/M-68K` が出ること、`bios.s` の `conout:` の行のブレークポイントで止まり、
  フレーム名 `conout` と行番号、行単位のステップを確かめます。

## 制約

- CP/M-68Kは1度に1つのプログラムしか動かないため、OS-9のようなスケジューラ・キューはありません。タイムラインはCCPとプログラムの切り替えだけです。
- BDOSの内部状態は呼び出しから学ぶため、ICEを途中から有効にした場合などは、次に該当する呼び出しがあるまで「—」です。
- ドライブはA:だけです。割り込みは `CONIN` の待ちにだけ使います。
- Alcyon Cのソースレベルデバッグは範囲外です。
