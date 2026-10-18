# Windows 原生1080p / 60秒流式探针

**状态：release构建的真实1080p/60秒受控源功能与本次音频供给预算验证通过；总体实时质量仍未关闭。** 本轮保留视频积压淘汰、最大90.4ms消费完成间隔和约5.16秒顺序定位开销，不将内容正确等同于稳定屏幕30fps。正常KaiCut播放器、UI、项目schema与MCP不切换；没有Present、可见窗口、桌面控制或录音，实际PCM仍输出到自有静音session。

## 固定范围

- 使用既有已审计LGPL-2.1-or-later FFmpeg7.1/ffprobe，生成60秒1920×1080/30fps动态testsrc2 H.264视频和48kHz双声道WAV。编码使用本机已验证的`h264_mf`，不下载、重编译sidecar或增加codec。来源与许可见 [binaries/README.md](../src-tauri/binaries/README.md)、[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)；KaiCut自有代码保持Apache-2.0。
- 视频源和实际解码输出都须为1080p，不能把160×90源放大后称原生1080p验证。保存ffprobe的width/height及coded_width/coded_height；编码对齐padding与可见图像尺寸分开解释。
- 独立入口仍为`native_stream_probe`，显式`--hd-fixture-dir`选择单个60秒HD case；旧默认160×90、4/20秒profile保留。
- 本轮运行release优化构建，renderer采用shared生命周期，音频采用event-mmcss。与旧debug结果不构成只改变分辨率的对照，程序/构建方式必须写入报告。
- 正常流程播放到约1秒后暂停200ms，再Resume到60秒EOF；然后重启decoder顺序seek30秒→播放到约31秒，再取消。称“完整60秒源覆盖”，不称无暂停连续播放，更不是10分钟、跨设备或复杂多轨压力矩阵。

## 帧大小与内存边界

1080p RGB24单帧为6,220,800字节，4帧视频解码queue为24,883,200字节（约24.9MB，23.7MiB）。这只是该队列的活动内容上限：

| 额外持有处 | 必须记录或说明的内容 |
| --- | --- |
| pipe reader与driver | reader当前帧、next/current帧及Busy重试引用；区分Arc共享引用和真正副本 |
| 渲染队列/worker | pending2、in-flight1及latest completion1；旧epoch等待完成的资源仍需计入 |
| RGB→RGBA及读回 | RGBA单帧8,294,400字节，转换、mapped去padding和验证可能有临时副本 |
| D3D资源 | 上传纹理、render target、staging等GPU/CPU可访问资源，不包含在RGB queue数字中 |
| 系统和decoder | FFmpeg内部、OS pipe和WASAPI缓冲，不能写作0或假定由queue计数精确控制 |

宽高、stride、帧偏移和分配乘法必须checked且有字节上限。`count bounded`不等于低内存；报告如没有实测RSS/VRAM，就只报告理论持有范围和未知项，不虚构进程峰值。

60秒1800帧的RGB总量约11.2GB，禁止把整段raw输出存盘或读入Vec作为reference。应用trace、每帧hash/PTS表及日志也有显式上限，不能靠丢弃失败记录维持“有界”表象。

## 独立reference ledger

用同一审核sidecar做独立参考解码，RGB pipe每次只处理一个完整1080p帧，记录index、真实decoded PTS/time_base与hash；处理完立即释放像素块，不保留整段RGB。参考与播放decode均需固定同一stream、像素格式、尺寸和passthrough时序，RGB完整帧和逐帧PTS一一配对，两侧EOF及子进程exit0都确认后ledger才有效。

ledger不使用`index % 120`复用旧4秒样本，也不以平均fps伪造源PTS。受控30fps夹具可以用预期`index/30`额外检查真实PTS，但原始时间戳、归零原点与实际内容hash仍保留。最后一帧index1799；完整帧数量1800，缺帧、重复/倒退PTS、partial frame或非零退出都不能形成有效reference。

参考生成与独立hash校验在播放之前或之后明确分隔，不在音频供给热路径执行整文件参考I/O。播放完成后核对实际消费的每个图形结果与该ledger，完整PCM及seek区间按实际提交序列核对。因为ledger也是同一FFmpeg构建独立解码，不将其称为不同解码器交叉验证。

## 诊断渲染不是生产播放架构

当前D3D路径每帧进行CPU RGB→RGBA、上传、离屏Draw、GPU→CPU readback及SHA校验。1080p下这些步骤比160×90昂贵，可能让渲染消费落后；有界latest-wins会丢弃积压任务，但PCM供给线程不得等待GPU或参考校验。

报告必须区分：

- 源解码帧数与真实PTS数量。
- 选择/提交、Busy拒绝、pending淘汰、rendered、published和主线程consumed数量。
- 已消费结果的不同源index及重复seek覆盖；不能把重复任务相加为不同帧。
- 旧epoch拒绝、阶段首帧/末帧覆盖和长时间无有效图形完成的观察。
- CPU上传/Draw/等待/Map/readback/校验耗时，而非纯GPU执行时间。

即使全部1800帧都完成离屏读回，也不能称为1800帧“已显示”或屏幕30fps；本轮没有swap chain/Present。若只完成部分帧但末帧达到，功能与队列生命周期可能成立，渲染吞吐仍须如实报告，不能用末帧掩盖中段停顿。

## 功能与实时门槛

完整HD源结束必须同时满足：

1. 音频2,880,000帧实际提交，完整f32输入序列hash与参考一致；不补假静音尾部。
2. 音频/视频/PTS管道干净EOF、自有进程正常退出，待提交块耗尽。
3. 真实WASAPI时钟映射到60秒且padding=0。
4. 当前epoch的源视频1799完成实际GPU验证；来源index/PTS/RGB及RGBA读回正确。

seek30秒以48kHz音频第1,440,000帧及视频900附近的真实PTS定位；本轮仍重启decoder从头顺序跳过，不能宣称常驻decoder或长GOP快速seek。取消后音频Stop/Reset、decoder kill/wait与reader join、renderer epoch退役和最终一次shutdown均明确记录。

供给质量按实际event buffer而非旧poll100ms预算判断；记录最大pump gap、处理耗时、实际等待、事件timeout、可靠时钟覆盖、下溢观察及主线程诊断开销。MMCSS每个实际播放阶段恢复，源hash前后一致。长phase截止时间和trace上限需适配60秒，不能延用只容纳20秒的参数，也不能因deadline放宽就将实时门槛放宽。

`functionalPassed`、供给预算及总体实时质量继续分列。旧154.3662ms供给gap、2754.2575msGPU回收与353–521ms Drop仍开放；更高负载下一次成功不抹去它们，失败也保留原始报告。是否追加一次重复，由首轮证据决定，不预先保证30fps或完全无丢帧。

## 运行入口

本轮HD使用release构建；直接Cargo示例可避免把`--release`错传给probe参数：

```powershell
cargo run --release --locked --manifest-path src-tauri/Cargo.toml --features native-stream-probe --example native_stream_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-hd-validation' --hd-fixture-dir '<已生成60秒1080p素材与reference ledger目录>' --renderer-lifetime shared --audio-scheduling event-mmcss
```

`--hd-fixture-dir`选择单个HD case；该目录需在运行前显式准备并校验，探针不隐式下载输入。旧profile、默认CLI语义和输出必须位于本仓库target内的新run目录规则保持。实际文件名、ledger字段与完整命令随实现和运行报告记录，本文不把尚未执行的命令当作通过证据。

## 本轮结果与后续

本批使用同一release程序先跑旧160×90回归，再跑一次HD。两份原始报告均退出码0、功能通过，供给预算通过；`realtimePerformanceQualified=false`始终保留：

- [HD原始报告](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/run-88a7a037-e1c0-45b9-a40e-280e6b056f35/native-stream-validation.json)。
- [旧流程回归报告](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/run-c45a2c4e-da2f-4e8e-8c32-d56c58c6ecb0/native-stream-validation.json)。
- [独立核验汇总](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/verification-summary.json)、[程序与源码身份](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/build-context.json)、[检查结果](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/verification-checks.json)。

源素材和参考位于`src-tauri/target/native-hd-validation/fixtures-b0282400-4252-4919-af85-4cc4b6a59450/`，`fixture-source.json`和`commands.json`保留生成命令、工具哈希、源探测及exit0。视频为原生testsrc2运动图，H.264、1920×1080、30fps、1800帧；coded_width/coded_height本机也均为1920×1080。视频SHA-256为`656568cd637a0d8bec6c835f6a4e8796bb759077dfec3c0a1c6e0514d2cd671f`。音频由已验证的4秒双声道PCM参考重复15次组成，WAV格式实际核验48kHz/2ch/PCM16，非根据文件大小猜测时长。

独立参考解码在32.84秒内处理11,197,440,000字节RGB管道，只保留一个RGB帧、一个RGBA行和小型hash表；未写整段raw文件。`video-reference.json`为474,409字节，SHA-256为`2da367deef3ef71c5a427d9ad7493f3c5fedb2bc7cf99c30c78f7d83b0e6ee9b`。播放后所有1820条实际消费结果按真实index/PTS/RGB/预期RGBA校验，无旧4秒索引取模。它仍是相同FFmpeg构建的独立解码参考，不是不同解码器的交叉验证。

### 设备与完整源覆盖

| 实测项 | 结果 |
| --- | --- |
| 图形设备 | Intel Arc硬件、D3D11 feature level 11_0，无WARP |
| 源/解码输出/renderer | 均1920×1080 |
| 音频输出 | 自有静音session，48kHz双声道float32，MMCSS Audio/NORMAL |
| 实际事件缓冲 | 1056音频帧，即22ms；请求duration/periodicity为0/0 |
| Start前应用reader读取 | 5760音频帧、1个RGB帧；未整段预载 |
| 完整提交PCM | 2,880,000帧，序列hash与参考相同 |
| 完整解码视频 | 1800帧，末帧1799完成GPU验证 |
| 完整源设备时钟/末尾padding | 60秒 / 0 |
| initial＋resume经过时间 | 60056.0274ms，另有200ms暂停观察 |
| 最大PCM供给间隔 | 10.5731ms，小于22ms缓冲预算 |
| 事件信号/超时/下溢观察 | 6110 / 0 / 0，包含seek阶段 |
| 暂停/定位/取消 | 冻结、30秒源入点和取消清空通过 |
| 最终GPU释放 | 14.5469ms，其中backend析构14.1622ms |

三个播放阶段均显式恢复MMCSS，源文件指纹保持不变，6个HD自有decoder PID和探针结束后无残留。暂停后完整覆盖60秒源不等于无暂停运行；观察下溢为0不证明物理扬声器输出无glitch。视频没有Present。

### 视频吞吐与内存

初次完整播放实际消费1791个不同源帧；另有29次seek结果，总消费1820条，跨seek去重仍为1791。renderer累计提交1832项、实际渲染1822项、发布/消费1820项；8项pending积压被淘汰，2项pending被控制事件清理，2项旧epoch完成被拒绝。解码1800帧与末帧通过不能掩盖中段未消费的源帧。

Resume中最长90.3978ms没有新的消费完成结果；已消费样本的CPU上传/Draw/等待/读回P95为10.125ms，最大31.5619ms。该耗时不包含其后完整SHA/逐像素验证，也不是纯GPU时间、呈现时间或整条处理链吞吐。因此本轮不宣称屏幕稳定30fps或零丢帧。

1080p视频队列实际高水位为4帧、24,883,200字节。每500ms采样的探针进程工作集最大179,785,728字节（约171.46MiB），私有内存最大166,088,704字节（约158.39MiB）；采样时读取的系统历史峰值工作集最大188,940,288字节（约180.19MiB）。见 [进程采样](../src-tauri/target/native-hd-validation/batch-ea2c8e38-edc3-43a4-b941-fa862bf65db4/hd.process-monitor.json)。这些仅覆盖探针进程，不含FFmpeg子进程、VRAM和采样间隔内遗漏的细节，不能与队列字节上限等同。

### 开销与未关闭项

GPU创建81.3715ms；音频/decoder会话创建374.6026ms，之后预填/首帧准备31.2733ms。30秒定位仍通过重启后顺序解码跳过前缀，准备耗时5164.5038ms，明显不适合作为正式交互定位体验；本轮未声称快速随机seek。

本次最终GPU退出较快，不覆盖历史154.3662ms供给间隔、2754.2575msGPU关闭及353–521ms析构波动。本轮仅一个HD样本和一个旧流程回归，不是跨设备/长时压力资格认定；与旧debug数值也不是同条件性能对照。

### 检查结果

前端lint、231项测试和构建通过；默认Rust100项通过、6项既有环境测试忽略；探针82项通过、1项既有sidecar专项默认忽略。本轮未单独重跑这些ignored项，真实设备验证为上述两个release流程。release构建、Rust格式和diff检查通过。测试、内容核验与设备运行分别保留证据；target目录被Git忽略，程序/源码哈希记录包含当前未提交代码身份。

本轮仍是受控1080p/60秒离屏诊断。同步readback与逐帧hash是验证开销，不作为正式零拷贝renderer方案；后续应基于实测决定是否分离诊断和呈现路径，再评估更长、更多设备与实际显示。没有主应用替换，也不把本轮结果说成WebView暖启动时差已解决。
