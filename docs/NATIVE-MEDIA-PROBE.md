# Windows 原生解码媒体探针

日期：2026-10-16。**状态：本机4秒离线解码→真实PCM提交→WASAPI时钟驱动帧选择链路已通过本轮窄范围验证。** 上轮 [NATIVE-CLOCK-PROBE.md](./NATIVE-CLOCK-PROBE.md) 验证的是静音标记包；本轮实际解码PCM进入专属静音音频session，视频仍只按真实PTS选择，没有D3D呈现，也没有替换主应用播放器。

本轮仍是 feature 控制的独立 Rust example `native_media_probe`，`native-media-probe` 显式依赖 `native-clock-probe`，正常桌面构建不启用。它不修改主应用播放器、UI、`.kaicut` 或已有 FFmpeg 二进制，也不运行或控制用户桌面。

## 交付范围

- 输入限定为已经生成的4秒视频MP4与4秒音频WAV测试夹具；不使用用户录音/视频，不宣称覆盖任意媒体格式、长时项目或VFR。
- 用既有打包FFmpeg离线解码视频为160×90 RGB帧；用ffprobe读取同一视频流的逐帧 `best_effort_timestamp` 与 time_base，保留原始时间戳并换算成纳秒，不按平均fps伪造时间戳。
- 将音频解码/重采样为设备mix rate和声道数的PCM16 WAV，解析为f32样本，再按明确支持的设备格式封包。实际PCM字节写入WASAPI，`ReleaseBuffer` 的 flags=0，不用SILENT标志替换掉样本。
- 每次创建新的音频session GUID，仅把该探针自己的shared rendering session设为静音并读回确认。这样实际样本进入队列但不发出测试音；不修改endpoint音量、系统默认设备或其它应用会话，不capture/loopback录音。
- 通过真实 `IAudioClock::GetPosition/GetFrequency` 将播放位置映射到源游标，选择已有RGB帧对应的到期PTS。**没有D3D窗口、纹理提交或视频实际显示**，所选frame/PTS/hash只形成调度账本。
- 验证播放、Stop冻结、Resume、源游标重新定位及取消；检查源文件哈希/大小/修改时间不变。EOF只提交剩余真实样本，不能补假音频让设备时钟显得持续推进。

## 必须区分的三个坐标

| 坐标 | 单位/来源 | 使用范围 |
| --- | --- | --- |
| 原始设备position | `IAudioClock`定义的设备单位 | 必须除以同流`GetFrequency`求秒，不能假设等于PCM帧 |
| Submitted / padding | 音频帧，按device sampleRate解释，每帧含全部声道 | 统计真实已提交样本、可写容量、队列是否耗尽；Reset后从新epoch重新计数 |
| 源音频游标 / 视频PTS | 源PCM帧索引、原始时间戳经time_base转换的ns | 定位实际解码内容，并判断哪一RGB帧到期；不使用墙钟代替设备读回 |

设备时钟的项目/源映射继续使用宽整数、有理数与epoch：

```text
elapsedNs = floor((position - anchorPosition) × 1_000_000_000 / clockFrequency)
sourceTimeNs = sourceStartNs + elapsedNs
sourcePcmFrame = floor(sourceTimeNs × deviceSampleRate / 1_000_000_000)
```

以上乘法和加法必须检查溢出，不能中途改为JS不安全整数或误用采样率换算设备position。超出源末尾的设备时间单独记录，供内容选择的源游标最多到EOF；clamp不能覆盖原始超量值。数据窗口使用半开区间，EOF处不得索引下一帧或额外提交一包。

## 解码与帧对应约束

`ffprobe -show_frames`描述解码帧；`best_effort_timestamp`是解码器给出的最佳估计呈现时间，不等于未经调整的packet PTS。[F1] RGB原始输出与metadata必须选择同一个视频流，解码阶段不得加`-r`、fps滤镜或CFR复制/丢帧；`fps_mode=passthrough`保留每帧的时间关系。[F2]

验证 `RGB字节数 / (160 × 90 × 3)` 必须恰等于metadata帧数，且没有残余字节；每个条目的PTS、帧索引、RGB范围和hash可逐项关联。这个受控路径只适用于相同源、相同流、没有时序滤镜的两次解码；若数量不一致、PTS缺失/重复/倒退或超出预算，应失败，不能排序或补帧后继续声称对应准确。

PTS换算先保留首个原始timestamp与有理time_base。若归零，使用足够宽的有符号中间差值处理负起点，再检查乘法和范围；不能直接把负timestamp强转u64。本次独立音视频夹具具有明确共同起点，分别归零只验证这一受控场景，不支持据此宣称任意容器音轨偏移、edit list或编码器priming已处理。

音频以解码后实际frame_count确定EOF；重采样到设备rate可能涉及取整与滤镜延迟，不能单靠容器duration推断样本数。检查PCM16 WAV的RIFF长度、fmt类型、sampleRate、channels、blockAlign、data完整帧及有限大小；解析i16时以32768归一化为f32，不能把声道样本数当音频帧数。

1.5秒seek采用真实输出PCM sampleRate的整帧偏移，例如 `sampleRate × 3 / 2`；seek前先Stop并Reset，重置队列和clock，建立新epoch后从该精确索引提交。旧epoch样本拒绝。首次提交字节/样本与选定RGB PTS应留证，防止“时钟跳了但样本仍从头播放”。这次定位是在已解码内存中的源游标跳转，**不是新的视频/音频解码器随机seek实现**。

## WASAPI格式、静音与写入

`WAVEFORMATEXTENSIBLE`必须核对`cbSize>=22`、`SubFormat`和`wValidBitsPerSample`。`wBitsPerSample`表示容器位宽，不是有效精度；formatTag65534且32bit不能直接判断为float。当前只接收已实现的Float32或PCM16格式，扩展PCM24-in-32、未知subtype/validbits或不匹配blockAlign明确拒绝，不能按近似格式写入。[S1]

封包必须是交错的完整音频帧，元素数量可整除channels，样本有限且范围明确；`nBlockAlign=channels×containerBytes`须一致。每次写入`min(availableFrames, remainingRealFrames)`，非零GetBuffer和ReleaseBuffer同线程配对，ReleaseBuffer帧数与申请相同。[S5] 本轮flags=0，与上轮静音标记包区别在于：音频引擎收到的是实际PCM，静音发生在专属session音量层。

`ISimpleAudioVolume`作用于整个音频session，不是单个渲染buffer。隔离来自`IAudioClient::Initialize`传入的新session GUID；`SetMute`的event-context GUID只是事件来源标记，不能拿它代替session隔离。必须从本client GetService获取控制接口，在Start前SetMute(true)并GetMute读回，设置失败就不启动。`NOPERSIST`防止该session静音/音量跨重启持久化，不能描述成“未修改任何静音状态”。[S2][S3][S4]

## EOF、停止与取消的判据

完成条件同时要求所有真实PCM帧已提交、当前padding=0以及设备时钟映射已达到真实源EOF。padding=0单独只表示该client队列已经被音频引擎消费；补充clock到达EOF避免在引擎已取完但设备时间尚差一小段时提前Stop。即使三项满足，仍**不是扬声器已物理播放最后一个采样的证明**。报告保留submittedFrames、源提交游标、paddingFrames、原始devicePosition/frequency以及源末尾超量。[S6][S7]

流继续Running但没有新PCM时，设备时钟可能走过真实PCM总时长。不能把`clockSeconds > submittedFrames/sampleRate`直接当成越界提交，也不能因此补SILENT包或重复尾样本。是否曾发生中途供给不足应通过填充记录/padding变化单独报告；时钟读回与队列耗尽指标不能互相冒充。

Stop后检查设备position冻结；Resume保留未消费队列与源提交游标，不能重复提交暂停前已经排队的PCM。取消流程为Stop→等待冻结观察→Reset，要求padding=0、clock=0，并使旧epoch不能污染新状态。`resourcesReleased`仅表示程序显式关闭和Rust资源释放/正常退出，不是系统级泄漏证明，也不能宣称共享Windows Audio服务已退出。[S7]

所有设备状态异常、S_FALSE不可靠采样、无前进/采样覆盖不足和解码错误都保留原始原因。若设备只提供不支持的mix格式，返回不支持；不更改设备配置以使探针通过。

## 固定运行步骤

1. 校验两份已生成源文件，记录来源路径、hash、长度和修改时间；读取/转换后再次核对。源时长必须符合此4秒窄夹具，不接收10分钟素材替代。
2. 离线解码视频RGB与逐帧PTS、音频PCM；建立设备格式转换后的实际frame_count和各字节范围。
3. 新专属静音session中预填真实PCM并Start，播放约2秒；Stop250ms确认时钟冻结。
4. Resume，按保存的提交游标继续填剩余真实PCM，直至所有源帧已提交、padding=0且设备时钟到达源EOF；记录最后设备位置及可能超出EOF的时长。
5. Stop/Reset，重新映射到源1.5秒、换epoch、检查旧epoch拒绝，按精确PCM offset重新开始，运行约1秒。
6. 取消：Stop后约150ms冻结观察，Reset后确认padding=0、clock=0，释放本探针资源；再次校验源文件完整性。

本轮目标是低风险短链路正确性，不是长时稳定性、GPU负载或多轨编辑性能测试。任何步骤实际失败都应保存报告，不能把其余步骤成功汇总成整条链路通过。

## 运行方法

本轮使用以下命令运行，可在仓库根目录复现：

```powershell
pnpm native:media-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-media-validation'
```

明确指定此前生成的4秒素材目录：

```powershell
pnpm native:media-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-media-validation' --fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\workflow-validation\98ee1285-41b7-452a-bbce-e38c83b1a2aa'
```

该目录也是未指定时的默认值，文件名为`原始 视频;[literal].mp4`与`原始 音频.wav`。文件名作为独立进程参数传递，不拼接shell。没有`--seconds`参数，步骤时长按固定4秒夹具执行。源目录缺失时明确失败，不下载用户媒体或自动改找其它目录。

别名对应：

```powershell
cargo run --locked --manifest-path src-tauri/Cargo.toml --features native-media-probe --example native_media_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-media-validation'
```

输出必须在本仓库`src-tauri/target`内，使用新的运行目录，禁止覆盖现有产物。默认build不运行此example；这些命令只用于明确选择的本地探针。

## 本机实测结果

最终原始报告：[native-media-validation.json](../src-tauri/target/native-media-validation/run-af68f7c9-9ddf-4d16-8f15-ac6f0b091629/native-media-validation.json)，配套源码/程序身份：[build-context.json](../src-tauri/target/native-media-validation/run-af68f7c9-9ddf-4d16-8f15-ac6f0b091629/build-context.json)。报告`ok=true`，共482条原始设备观察，全部可靠，未观察到S_FALSE。独立程序SHA-256为：

```text
785230FABA679BFBBE08FA60C9AE2FA0875CD9F82AA19D28977AB19766AC086E
```

| 输入/解码 | 实测 |
| --- | --- |
| 视频 | 120帧、每帧160×90 RGB；实际源time_base=1/15360，原首PTS=0，末PTS对应3,966,666,666ns |
| 视频时间覆盖 | 校验每帧实际PTS符合本次4秒/30fps窄夹具；不是仅凭“120帧”推断时长，也没有按fps伪造PTS |
| 音频 | 48kHz双声道、192000音频帧；PCM16 WAV解析后转换为f32样本 |
| 设备格式 | float32、validBits32、blockAlign8、buffer4800帧；clockFrequency384000单位/秒，与48000Hz采样率分别使用 |
| 自有session | `sessionMuted=true`；实际PCM通过flags=0提交，不以SILENT包代替内容 |
| 完整提交 | 192000帧恰提交一次、其中287520个标量输入样本非零；`fullPcmSubmissionVerified=true` |

完整提交的f32输入字节SHA-256为`66be5f80f8dcdcdf4164a2030a788f356ceb62d216fd2eacf545104f8cfcee3e`，与完整解码源转换结果核对一致；解码PCM16 WAV本身的文件SHA-256为`17d7595697308e97595a0db4564d8d2b3561f4797ee7b787b3709590f418f41e`。两者是不同表示的哈希，不能互换。EOF没有追加零尾巴或重复尾样本。

| 阶段 | 设备与内容证据 |
| --- | --- |
| Start至约2秒 | 源时间到2.007250秒；随后Stop250ms，原始设备位置前后均779520，padding保持4800 |
| Resume至EOF | 全部192000帧已提交；device position1536000 / frequency384000=4秒，padding=0，`clockReachedSourceEnd=true`；观察到的中途队列耗尽次数为0 |
| 完整视频选择 | Start和Resume合并去重后覆盖帧0–119，共120个实际解码帧；阶段计数61和60在暂停边界共享一帧，不能相加声称121个不同帧 |
| Reset / 源游标1.5秒 | PCM起点为第72000帧，首个选择视频帧为45、实际PTS=1.5秒；`seekPcmSubmissionVerified=true`，提交内容哈希匹配该源切片 |
| 再播约1秒后取消 | 源时间到2.508083333秒；Stop150ms前后raw position均399360、padding保持3840；Reset后设备位置与padding均0 |
| epoch | 初始1→重新定位2→取消后3；旧epoch读回被拒绝 |

所有阶段的视频条目均为`videoPresented=false`。选择帧只是以真实设备位置决定哪个已解码RGB/PTS当前到期，不代表GPU已经显示了该帧。`underrunObservations=0`是本次轮询观察结果，不作为所有设备/任意负载绝无下溢的证明。

原始与复制的MP4/WAV的hash、长度、修改时间前后一致，`sourceIntegrityVerified=true`。`resourcesReleased=true`表示完成显式Stop/Reset、Rust Drop及正常进程退出；本轮没有做系统级COM/句柄泄漏测量。没有开启UI、录音或可听测试音，主应用仍运行原来的WebView预览路径。

### 本轮纠正的验收问题

中间一次运行观察到padding已经为0，但音频设备时钟仅到约3.9847秒。该结果说明“音频引擎取完队列”不等于设备时钟已经覆盖4秒源区间。最终实现增加“全部真实帧已提交＋padding=0＋设备时钟到EOF”联合条件，并以回归测试固定；最终报告达到4秒后才停止。原始设备时间与clamp后的源时间分开记录，不通过补静音掩盖尾部覆盖问题。

另对视频实际时间范围、PCM完整提交和1.5秒源切片增加验证，避免只看帧数量或源游标数字就误判内容正确。离线解码仍有固定时长/内存预算，本轮不是流式解码实现。

### 命令验证与兼容回归

- 普通Rust回归：100项通过、6项既有环境用例默认忽略。
- `native_media_probe` example测试：28项通过；`native_clock_probe` example测试：21项通过。两者包含共享模块的重复测试，不能相加称为49项独立用例。
- 前端231项测试、lint、前端build和Rust格式检查通过。
- 对共享WASAPI模块做旧静音探针设备回归：3次生命周期循环＋2秒连续采样，报告见 [clock-regression](../src-tauri/target/native-media-validation/clock-regression/run-a594faf5-db2b-490f-b1cd-6e2025de179e/native-clock-validation.json)。
- 本轮没有MCP代码改动，未重复跑MCP协议测试；没有重新生成Tauri安装包，因为主应用播放器未被替换。

## 下一阶段

本轮修改文件：新增 `src-tauri/examples/native_media_probe.rs`、`examples/support/decoded_media.rs`；扩展共享 `examples/support/wasapi_output.rs` 并为旧 `native_clock_probe.rs` 标明共享接口使用范围；`src-tauri/Cargo.toml`/`package.json` 增加非默认探针入口；README、架构、任务、原生命令契约及本文更新。既有FFmpeg二进制、主播放器与项目schema没有更换。

下一步在独立探针中验证实际D3D纹理上传、读回与渲染时序，检查输出RGB与解码内容对应，再测与WASAPI共享时钟的PTS排期；之后再评估与主编辑器adapter整合。当前RGB账本不能说明屏幕呈现同步，专属session静音也不能证明听感或物理音频延迟，**WebView约49–56ms暖启动偏差仍未因此修复**。

## 一手来源

- [S1] [Microsoft WAVEFORMATEXTENSIBLE](https://learn.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible)：容器位宽、validbits、SubFormat及声道含义。
- [S2] [Microsoft ISimpleAudioVolume](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-isimpleaudiovolume)：会话级音量/静音作用范围。
- [S3] [Microsoft ISimpleAudioVolume::SetMute](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-isimpleaudiovolume-setmute)：event context只表示事件发起来源。
- [S4] [Microsoft IAudioClient::Initialize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize)：AudioSessionGuid决定所属会话、NULL/GUID_NULL语义。
- [S5] [Microsoft IAudioRenderClient::GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-getbuffer) 与 [ReleaseBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-releasebuffer)：完整帧、容量、配对与SILENT行为。
- [S6] [Microsoft IAudioClient::GetCurrentPadding](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getcurrentpadding)：排队未消费帧数，区别于物理输出延迟。
- [S7] [Microsoft IAudioClock::GetPosition](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getposition) 与 [GetFrequency](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getfrequency)：设备单位、Stop冻结/Reset归零与QPC单位；详细核对见前一探针文档。
- [F1] [FFprobe官方文档](https://ffmpeg.org/ffprobe.html)：show_frames、show_entries与流选择的原始说明。
- [F2] [FFmpeg官方文档](https://ffmpeg.org/ffmpeg.html)：fps_mode passthrough/CFR区别、copyts与时间戳处理边界。

本轮已实际读取新增的Microsoft会话/格式/队列页面及FFmpeg官方页面，均返回HTTP200；来源用于核对契约，不能替代本机设备运行结果。
