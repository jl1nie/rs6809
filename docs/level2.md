# Multicomp NitrOS-9 Level 2

## 再現

参照キットはREADMEに記載のコミット `bf60370cfa180636cde3ee305d089d94872de82a` を使用します。
ビルドスクリプトはこのリビジョンを確認し、外部ソースへのパッチは `artifacts/level2/sources/` 内のコピーに適用します。
カーネルやROMのバイナリはリポジトリへ追加しません。

```sh
python3 scripts/build-level2-disk.py
sh examples/nitros9-level2.sh
cargo test --release --test nitros9_level2 -- --ignored
```

検証した経路はCamelForth→Level 2カーネル→SysGo→startup用Shell→echo→対話Shell→mfree→echo→対話Shellです。
`mfree` は物理メモリの上限 `$7FFFF` と440KBの空きを表示しました。
これは最小ディスクでの結果で、搭載モジュールや実行中プロセスによって空き量は変わります。

```text
NitrOS-9/6809 Level 2 V03.3.0
...
LEVEL2-STARTED
...
OS9:mfree

 Blk Begin   End   Blks  Size
 --- ------ ------ ---- ------
   9  12000  7FFFF   37   440k
                   ==== ======
            Total:   37   440k

OS9:echo LEVEL2-DONE
LEVEL2-DONE

OS9:
```

## エミュレータのMMU

CPU側のアドレスは16ビットのままです。`Machine::multicomp()` の物理RAMを512KBに拡張し、
8KB×64バンクを選択できます。`multicomp_with_ram()` / `--ram-kb` では
64・128・256・512・1024KBを選択でき、標準のフラットマシンは64KBのままです。

- `$FFDE`：bit7=ROMDIS、bit6=実行タスク、bit5=MMUEN、bit3–0=書き換えるマップ番号。bit4が1の制御書き込みは無視します。
- `$FFDF`：bit7=書き込み保護、bit6–0=物理バンク番号。タスクごとに8個のマップを保持します。
- MMU無効時は下位64KBを直接アドレスします。未搭載バンクの読み取りは `$FF`、書き込みは無視します。
- MMUENとROMDISがともに1の状態から、MMUENを保ちROMDISを0にする書き込みで固定上位領域を有効にします。
  ROMは無効状態を維持します。MMUを無効化すると固定上位領域のラッチを解除します。
- 固定上位領域 `$FE00–$FFFF` は物理バンク7の末尾512バイトです。
  タスクやスロット7の保護・未搭載バンク指定にかかわらず読み書き可能で、I/Oが優先します。

参照：キットの `exec09/miscsbc.c`、`defs/mc09.d`、`level1/coco1/modules/rel.asm`。
実装はマッピング表の再計算をレジスタ変更時に限定しています。

## Level 2の起動イメージに必要な調整

RAMの拡張だけでは、参照キットのLevel 2ソースをそのまま起動できませんでした。
`scripts/nitros9-level2.patch` に以下をまとめています。

1. RELのパディングを戻して304バイトにし、BOOTを464バイト、KRNのロード先を `$F000` に揃えます。
2. 参照ソースの更新で変わったシンボル名（`InstallSvc` / `FSFreeLB`）と分岐距離を合わせます。
3. `F$LDDXY` / `LDAXY`、タスク設定、`F$Move` に残っていたCoCoのDAT直接操作を
   Multicompの `$FFDE–$FFDF` 操作に置き換えます。2バイト読み取りは8KB境界をまたげます。
4. `F$Move` はソース・転送先をマップしている間にスタックを使いません。
   カーネルのレジスタフレーム転送もこのルーチンを使用します。
   プロセス記述子やシステムスタックが `$A000–$DFFF` に割り当てられると、従来の転送は
   転送元をマップから外してしまい、子プロセスの起動・システムコール復帰に失敗しました。
5. SDドライバのLSN0判定を加算からORへ変え、非ゼロのLSNが桁あふれでLSN0と誤判定される問題を防ぎます。
   既存のC開発用ディスクと同じ修正です。

アセンブル時はLevel 2用の `defsfile` を置いた生成ディレクトリでソースを処理します。
Level 1のソース相対インクルードをそのまま使うとLevel 1のシステム変数オフセットが混入します。
RBFはLevel 2版を使用します。最小ディスクの収録コマンドはShell 2.1、echo、mfreeです。

## 検証範囲

`tests/memory_map.rs` では全RAM容量の全物理バンク、未搭載バンク、書き込み保護、
2タスクの独立性と共有RAM、固定ベクタ領域、ROM切り替え条件を検証します。
既存のLevel 1→BASIC09→`42` の起動テストも512KB構成で実行します。

この構成はMulticomp向けNitrOS-9 Level 2です。CoCo 3のGIME・画面・キーボード・専用ディスクは対象外です。
1MBのバンク動作はMMUテストで検証しますが、このカーネルのメモリ管理は512KB向けです。
BASIC09やCコンパイラをLevel 2ディスクへ追加する作業は含みません。
ローカルコンソールの既定モードではディスク差分は終了時に消えます。`--persist-disk` またはTelnetモードでは永続化します。
マルチユーザー構成は [Telnet接続の説明](telnet.md) を参照してください。
