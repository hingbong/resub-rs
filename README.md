# resub

字幕批量改名 + MKV 内嵌字幕抽取。**单个 Rust 可执行文件，无任何运行时依赖** ——
不需要 .NET Framework、JRE、Qt，也不需要浏览器。

是「U2字幕重命名工具 1.0 Final」（.NET 2.0 WinForms）的替代实现，并额外支持把
Matroska 里的内嵌 ASS/SSA 抽成独立文件。

## 下载

Windows 直接下 [nightly release](https://github.com/hingbong/resub-rs/releases/tag/nightly) 里的
`resub.exe`，每次 push 到 main 自动重新构建并覆盖同一个 release。
双击即用，不需要安装任何运行时。

NAS 用 `resub-linux-x86_64`：静态链接的 musl 二进制，不依赖系统的 glibc，
拷过去 `chmod +x` 就能跑。见下面「在 NAS 上跑」。

## 构建

只需要 [Rust](https://rustup.rs)：

```sh
cargo build --release
```

产物 `target/release/resub.exe`（Windows，约 4 MB），拷到任何机器双击即可运行。
不想自己编的话，直接下 nightly release 里的 `resub.exe`（每次 push 到 main 自动覆盖）：
https://github.com/hingbong/resub-rs/releases/tag/nightly

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

原生窗口，界面与逻辑在同一个进程里。选目录用的是系统原生对话框，
NAS 的映射盘符、UNC 路径和网络邻居都能直接选到。

### 命令行

```sh
# 预览（不动磁盘）
resub plan  --videos D:\动画 --subs D:\字幕 --suffix .chs

# 执行，把字幕复制到视频旁边
resub apply --videos D:\动画 --subs D:\字幕 --suffix .chs

# 就地重命名字幕
resub apply --videos D:\动画 --subs D:\字幕 --mode rename

# 抽取内嵌字幕：单个文件，或整个目录（递归子目录里的 .mkv）
resub extract 某集.mkv --out D:\字幕
resub extract D:\动画 --out D:\字幕
resub extract 某集.mkv --track 3
```

### 在 NAS 上跑（不想让视频绕一圈）

抽取内嵌字幕必须走完整个 MKV：Matroska 的 cluster 是所有轨道交错存的，字幕块散落
在每一个 cluster 里，没有一个能跳过 payload 的索引。所以在电脑上抽 NAS 上的片子，
等于把整部片子先拉一遍到本地。

把 resub 放到 NAS 上跑就没有这个问题：读的是本地磁盘，写回同目录，网络流量为零。

```sh
# 拷到 NAS 上（群晖例：/volume1/@appstore 或家目录都行）
chmod +x resub-linux-x86_64

# 整个目录递归抽取，字幕写在各自的 MKV 旁边
./resub-linux-x86_64 extract /volume1/video/动画 --skip-existing
```

`--skip-existing` 是给 NAS 准备的：输出名只从文件头部的 Tracks 元素推出来，
已经抽过的文件读几 KB 就能判定并跳过。一集 400 MB 的 MKV，第二次运行是毫秒级的：

```
$ ./resub-linux-x86_64 extract 某集.mkv --out /tmp/out
完成：1 个文件，2 条轨道写出，0 条已存在跳过，0 个文件失败      real 1.85s
$ ./resub-linux-x86_64 extract 某集.mkv --out /tmp/out --skip-existing
完成：1 个文件，0 条轨道写出，2 条已存在跳过，0 个文件失败      real 0.002s
```

想定时跑就丢给计划任务（群晖：控制面板 → 任务计划），每天扫一遍：

```sh
./resub-linux-x86_64 extract /volume1/video --skip-existing
```

### 让 Windows 窗口一键驱动 NAS

Windows 上的 `resub.exe` 可以直接 ssh 到 NAS 上，用那边的一份二进制干活。
**NAS 上不需要常驻进程、不用开端口，NAS 重启也不影响** —— 每次操作就是一条 ssh 命令，
NAS 本地读盘、本地写 `.ass`，只把 JSON 结果传回 Windows。

一次性准备（Windows PowerShell，只做一次）：

```powershell
# 1. 生成密钥（已有就跳过）
ssh-keygen -t ed25519 -N '""' -f $env:USERPROFILE\.ssh\id_ed25519

# 2. 把公钥装到 NAS（会让你输一次密码）
$OutputEncoding = New-Object System.Text.UTF8Encoding $false
Get-Content $env:USERPROFILE\.ssh\id_ed25519.pub -Raw | ssh -p 2222 用户名@192.168.1.10 "mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"

# 3. 验证免密（不再问密码就对了）
ssh -p 2222 用户名@192.168.1.10 "echo ok"
```

第 3 步如果还在问密码，先看公钥有没有被 BOM 污染：

```sh
ssh -p 2222 用户名@192.168.1.10 "head -c 3 ~/.ssh/authorized_keys | od -An -tx1"
```

开头出现 `ef bb bf` 就是 BOM。要说明的是：BOM 不是 `Get-Content` 产生的，而是
PowerShell 往原生命令的管道里写字节时按 `$OutputEncoding` 编码造成的（5.1 默认是
ASCII，7+ 默认是无 BOM 的 UTF-8，但只要有人为了治中文乱码把它设成带 BOM 的 UTF-8，
每次管道都会带上）。所以第 2 步先把 `$OutputEncoding` 显式设了一遍。用 `-Raw`
读整个文件、而不是逐行读，也能少一层行尾和编码的意外。

然后打开 resub.exe，「抽取 MKV 内嵌字幕」里：

1. **NAS 地址** 填 `用户名@192.168.1.10:2222`。
   端口可以省略：地址里不写端口时 resub **不传 `-p`**，让 ssh 自己读 `~/.ssh/config`。
   所以已经配过 `Host nas` / `Port 2222` / `User yukine` 的话，这里直接填 `nas` 就行。
2. **本地程序** 可以留空。留空时「部署」会先用 `curl` 从 nightly 拉一份
   `resub-linux-x86_64` 到临时目录，再传上去。
3. 点「**部署**」—— 上传 + `chmod +x` + 验活，一步完成（升级版本再点一次）。
4. 点「**连接**」确认可用。没部署过会直接告诉你「还没有 resub，先点部署」，
   而不是甩一句 `No such file or directory`。
5. 「**远程浏览…**」列的是 NAS 上的目录；选好路径点「**在 NAS 上抽取**」。

设置会记住：NAS 地址、远程程序路径、本地二进制、上次浏览到的目录、跳过已存在的勾选，
下次打开窗口就是上次的样子。存在 `%APPDATA%\resub\config.json`。

「**远程浏览…**」里：

- 点目录名 → 进去
- 点目录后面的「**选这个目录**」→ 直接把这个目录整个作为输入，不用进去再退出来
- 点文件名 → 选用该文件
- 顶部「**✓ 用这个目录**」→ 选当前所在目录
- 列表只显示子目录和视频/字幕文件，其他文件不列
- **空目录**和**读不出来**是两种提示，不会都显示成一片空白

列不出目录、连不上的时候点「**诊断**」，它用一条 ssh 命令把这些全打出来，直接看是哪一步不对：

```
user=...
home=...
--- ls -ld $HOME ---
--- ls -ld /volume1 ---
--- ls /volume1 ---
--- ls -l <远程程序> ---
--- run ---
```

**远程程序** 默认 `~/resub-linux-x86_64`（群晖上会展开成
`/var/services/homes/<用户名>/...`）。家目录不可写、或者想放到别的盘，就改成绝对路径。

地址留空 = 本机抽取；填 `http://...` 则退回连一个已经在跑的 `resub serve`。

部署只需要做一次（升级版本时再做一次）。

上传**不走 scp**：OpenSSH 9 起 scp 默认改用 SFTP 协议，而群晖这类 NAS 的 sshd 通常没配
`Subsystem sftp`，会直接报 `subsystem request failed on channel 0`。resub 改成用普通
ssh 会话的 stdin 写过去（`cat > 目标路径`），只要 ssh 能登录就不需要服务端任何开关，
传完再比对字节数和试跑一次。实测这条链路（用 shim 模拟 ssh 跑通）：

```
probe  -> Ok("me@nas.example 上的 resub 可用")
ls     -> path=Some("/workspace/downloads") files=3
extract-> 2 tracks [449, 447]
skip   -> 2 tracks, all skipped = true
deploy -> Ok("已上传到 yukine@nas.example:/tmp/fakeremote/deployed（32293040 字节）")
          传完的 sha256 与本地一致
```

### 浏览器界面（可选）

如果不想用原生窗口，也可以用浏览器当界面：

```sh
resub serve --port 8080
```

文件读写全部由 Rust 进程完成，浏览器只负责显示，所以 **Firefox 也能用**
（不依赖 Chromium 独有的 File System Access API）。

默认只监听 `127.0.0.1`。在 NAS 上跑、想从别的机器打开界面，就加 `--host 0.0.0.0`：

```sh
./resub-linux-x86_64 serve --host 0.0.0.0 --port 8080 --no-open
```

然后在 Windows 浏览器打开 `http://群晖IP:8080`。注意这个界面没有鉴权，
只在自家局域网里这么用。

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
