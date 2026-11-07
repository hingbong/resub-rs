# resub

字幕批量改名 + MKV 内嵌字幕抽取。**单个 Rust 可执行文件，无任何运行时依赖** ——
不需要 .NET Framework、JRE、Qt，也不需要浏览器。

是「U2字幕重命名工具 1.0 Final」（.NET 2.0 WinForms）的替代实现，并额外支持把
Matroska 里的内嵌 ASS/SSA 抽成独立文件。

## 构建

只需要 [Rust](https://rustup.rs)：

```sh
cargo build --release
```

产物 `target/release/resub.exe`（Windows，约 4 MB），拷到任何机器双击即可运行。
不想要图形界面的话：

```sh
cargo build --release --no-default-features   # 纯 CLI，体积更小，不依赖 OpenGL
```

## 用法

### 图形界面

直接双击 `resub.exe`，或：

```sh
resub gui
```

原生窗口，界面与逻辑在同一个进程里。

### 命令行

```sh
# 预览（不动磁盘）
resub plan  --videos D:\动画 --subs D:\字幕 --suffix .chs

# 执行，把字幕复制到视频旁边
resub apply --videos D:\动画 --subs D:\字幕 --suffix .chs

# 就地重命名字幕
resub apply --videos D:\动画 --subs D:\字幕 --mode rename

# 抽取内嵌字幕
resub extract 某集.mkv --out D:\字幕
resub extract 某集.mkv --track 3
```

### 浏览器界面（可选）

如果不想用原生窗口，也可以用浏览器当界面：

```sh
resub serve --port 8080
```

文件读写全部由 Rust 进程完成，浏览器只负责显示，所以 **Firefox 也能用**
（不依赖 Chromium 独有的 File System Access API）。

## 和原版的差别

| | 原版 ReSubName | resub |
|---|---|---|
| 运行时 | .NET Framework 2.0 | 无，单 exe |
| 集数识别 | 去掉非数字后取数 | 分层匹配 `第N話` / `SxxExx` / ` - NN` / `[NN]`，排除 1080p / x264 / 年份 |
| 语言后缀 | 手动从下拉框选 | 原名已带 `.sc`/`.tc`/`.SumiSora.tc` 等标记时自动沿用 |
| 冲突处理 | 跳过 | 标为「目标已存在」并跳过，预览里可见 |
| 抽取内嵌字幕 | 不支持 | 支持，含 zlib 压缩轨道 |

## 为什么能读某些播放器读不了的字幕

部分压制组用 Matroska 的 `ContentCompression` 压缩字幕轨道，且**省略 `ContentCompAlgo`**。
按规范省略即 **0 = zlib**，但 Media3/ExoPlayer 的 `MatroskaExtractor` 只在算法为 3
（header stripping）时处理，其余情况会静默产出 0 条字幕。resub 按规范默认 zlib。

已验证：对 LoliHouse 某集的双中文 ASS 轨道，抽出 449 / 447 条事件，与独立
Python 参考实现逐行一致。

## 已知限制

- lacing 编码的字幕块会被跳过（实际中字幕块几乎不用 lacing）
- 加密的字幕轨道不支持
- SRT 轨道导出为 `.srt`，不会转换成 ASS
- 界面字体优先使用系统里的 Noto Sans；找不到时回落到系统字体提供器（Windows 上即微软雅黑），中文都不会缺字
