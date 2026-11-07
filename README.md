# resub

字幕批量改名 + MKV 内嵌字幕抽取。单个 Rust 可执行文件，**无运行时依赖**，不需要装 .NET / JRE / Qt。

是 [U2字幕重命名工具 1.0 Final](https://github.com/)（.NET 2.0 WinForms）的替代实现，
并额外支持从 Matroska 里把内嵌 ASS/SSA 抽成独立文件。

## 构建

只需要 [Rust](https://rustup.rs)（Windows 上装 MSVC 工具链即可）：

```sh
cargo build --release
```

产物是 `target/release/resub.exe`，约 2 MB，拷到任何 Windows 机器直接运行。

## 用法

### 图形界面

```sh
resub gui
```

会在 `127.0.0.1` 起一个本地服务并打开浏览器。页面只负责显示，**所有文件读写都在 Rust 进程里做**，
所以 Firefox 也能用 —— 不依赖 Chromium 才有的 File System Access API。

- 批量改名：选视频目录和字幕目录 → 预览 → 执行
- 抽取字幕：选 MKV → 抽成 `视频名.chs.ass` / `.cht.ass`

`--port 8080` 指定端口（默认随机），`--no-open` 不自动开浏览器。

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

## 和原版的差别

| | 原版 ReSubName | resub |
|---|---|---|
| 运行时 | .NET Framework 2.0 | 无，单 exe |
| 集数识别 | 去掉非数字后取数 | 分层匹配 `第N話` / `SxxExx` / ` - NN` / `[NN]`，并排除 1080p / x264 / 年份 |
| 语言后缀 | 手动从下拉框选 | 原名里已带 `.sc`/`.tc`/`.SumiSora.tc` 等标记时自动沿用 |
| 冲突处理 | 跳过 | 标为「目标已存在」并跳过，预览里能看见 |
| 抽取内嵌字幕 | 不支持 | 支持，含 zlib 压缩轨道 |

## 为什么能读某些播放器读不了的字幕

部分压制组会用 Matroska 的 `ContentCompression` 压缩字幕轨道，且**省略 `ContentCompAlgo`**。
按规范省略即 **0 = zlib**，但 Media3/ExoPlayer 的 `MatroskaExtractor` 只在算法为 3（header stripping）
时处理，其余情况会静默产出 0 条字幕。resub 按规范默认 zlib，能正常解出。

## 已知限制

- lacing 编码的字幕块会被跳过（实际情况中字幕块几乎不用 lacing）
- 加密的字幕轨道不支持
- SRT 轨道可导出为 `.srt`，但不会转换成 ASS
