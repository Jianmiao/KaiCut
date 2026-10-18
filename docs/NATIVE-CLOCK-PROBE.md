# Windows 原生音频时钟探针

日期：2026-10-16。**状态：独立探针已构建，并在本机真实 WASAPI shared 输出端点完成短时验证。** 本次确认音频流时钟、Stop/Resume/Reset和项目坐标映射可用；未接入视频解码/呈现，不代表已解决 WebView 暖启动约49–56ms的音视频时差。

本探针回答一个窄问题：能否在 Windows 上取得真实 WASAPI 输出流位置，并在启动、停止、恢复、重置和项目时间重新锚定后，为视频 PTS 选择提供同一个时间来源。

它是独立 Rust example，通过 `native-clock-probe` feature 显式构建/执行；正常 KaiCut 桌面构建不启用该 feature，也不自动打开音频会话。不会替换当前 WebView 播放器，不改变 UI、Rust 项目权威状态、`.kaicut v2` 或既有 FFmpeg 构建。

## 范围与证据边界

- 打开默认渲染端点的 shared-mode 输出流，使用设备 mix format，持续提交标记为静音的数据包。这是真实输出设备/音频引擎路径，**不是播放可听测试音**。
- 不打开 capture/loopback 端点、不录音、不修改系统默认设备、系统或应用音量、采样率设置，不使用 exclusive mode，不控制桌面或其它软件。
- 输出流仍是系统可观察的临时音频会话。`AUDCLNT_STREAMFLAGS_NOPERSIST` 用于禁止该 rendering session 的音量/静音设置跨重启持久化；它不表示“没有创建音频会话”。[S10]
- 视频侧只有按 30fps 生成的合成 PTS 调度账本；根据真实音频位置决定当前到期的 frame/PTS，记录重复观察与跳过的 PTS。**不解码视频、不创建 D3D 纹理、不实际呈现视频。**
- 因此本探针可以验证设备时钟读回、生命周期及时间映射；不能证明音视频画面/声音已同步，不能测量屏幕光子或扬声器物理延迟，也不能直接关闭当前预览启动的一帧质量门槛。

## 已核实的 Microsoft API 契约

以下 Microsoft Learn 页面均在本次调查中成功读取。页面内容是设计依据，本机实测与其结论范围单独记录在下文。

| API / 字段 | 官方定义 | 本探针的处理要求 |
| --- | --- | --- |
| `IAudioClock::GetPosition` 的 position | 从流起点开始的设备位置；数值单位不固定，只能与 `GetFrequency` 搭配解释 | 始终以 `position / frequency` 求秒，不把 position 当 PCM frame，也不除以 mix format 的 sample rate |
| `IAudioClock::GetFrequency` | 与 position 单位兼容的每秒单位数；可能例如 bytes/sec，而非 frames/sec | 与采样率独立记录；频率为零拒绝计算；同一锚点下频率发生变化时显式失败/重新锚定，不静默套用旧单位 |
| `GetPosition` 的 QPC 输出 | 设备读位置时取得的相关性能计数器，**已转换为 100ns 单位** | 命名 `qpc100ns`，秒差除以10,000,000；不得再按原始 QPC frequency 换算一次 |
| `QueryPerformanceCounter` / `QueryPerformanceFrequency` | 前者返回 raw counts；后者返回 counts/sec，开机固定、跨处理器一致 | 若使用 raw QPC 交叉比较，先以宽整数执行 `raw × 10,000,000 / QPF`，再与 qpc100ns 比较；不直接相减 |
| `GetPosition` 返回 `S_OK` | 成功且位置读回精度符合该调用契约 | 可进入本探针有效时钟统计；仍不等于无设备/采样误差 |
| `GetPosition` 返回 `S_FALSE` | 调用成功，但读回延迟影响位置精度 | 保留 HRESULT 与 `reliable=false`；不纳入严格精度通过证据，允许有限重试，禁止无限循环 |
| `IAudioClient::Start` | 开始流/恢复时钟；初次或 Reset 后从零开始，Stop 后继续旧位置 | 开始前预填缓冲；Start 成功不等于 position 当场大于0，初期可以停在0几毫秒 |
| `IAudioClient::Stop` | 停止流并冻结音频流位置；之后 Start 从原位置继续 | Stop 后隔一段真实时间再次读 position，检查冻结；不要求配对 QPC 时间戳也冻结 |
| `IAudioClient::Reset` | 只能在 stopped 且没有未完成 buffer 读写时调用；清空待播数据并把流时钟位置归0 | 顺序为 Stop→完成 buffer 配对→Reset→新 epoch/rebase→预填→Start，不在 Running 时 Reset |

来源：[S1]–[S6]、[S11]–[S12]。`Stop` 的 `S_FALSE` 表示已停止；`Reset` 的 `S_FALSE` 表示已经 reset。这与 `GetPosition` 的 `S_FALSE` 含义不同，不能统一标成“采样不可靠”或 API 失败。[S4][S5]

`GetFrequency` 返回名义频率而不报告短时 drift/jitter；这些变化由实际 position 读回体现。因此把 position 换成秒只是单位换算，并不能由公式证明与系统时钟完全一致。[S2]

### shared-mode 缓冲与格式

`GetMixFormat` 返回由 COM allocator 分配的格式指针；通常指向 `WAVEFORMATEXTENSIBLE` 开头的 `WAVEFORMATEX`。应将完整原始结构交给同一设备的 shared-mode `Initialize`，不要只复制基础头丢失扩展字段；无需时使用 `CoTaskMemFree`，所有失败分支也需释放。[S6]

shared-mode 的 `hnsPeriodicity` 必须为0；请求 buffer duration 使用100ns单位，最终容量应读取 `GetBufferSize`，不能假设等于请求值。当前探针请求100ms缓冲，用实际返回帧数执行填充。[S7]

`GetCurrentPadding` 返回已排队待播放的**音频帧数**；每帧字节数由 `nBlockAlign` 定义。它不是 `IAudioClock` 的 raw position 单位。计算 `availableFrames = bufferFrames - paddingFrames`，请求不超过可用容量；padding大于容量应报告不一致，不靠无符号下溢产生大请求。[S8]

每次非零 `GetBuffer(N)` 必须在同一线程配对 `ReleaseBuffer(N, AUDCLNT_BUFFERFLAGS_SILENT)`；静音标志使音频引擎把整个包视为静音，不必人为写零。不要持有 buffer 跨越等待/Stop/Reset；Get/Release 应在同一处理周期及时完成。初始先填充，再 Start。[S3][S9][S13][S14]

### Rust HRESULT 与 COM 生命周期

本机 `windows-0.62.2` 的生成绑定中，`IAudioClock::GetPosition` 调用 raw vtable 后使用 `.ok()`，将 `S_OK` 与正值 `S_FALSE` 都变成 `Ok(())`。若使用该便捷方法，无法保留两种精度状态。本探针要求通过 `Interface::vtable` 取得原始 HRESULT，再明确分类，并保留负值失败码。[L1]

COM 初始化、端点/AudioClient/RenderClient/Clock 与 buffer 操作由同一专用执行线程持有；对象释放后才解除线程 COM 初始化。用资源管理确保错误路径也停止/释放自有流，不根据进程名结束其它音频程序。没有设备、被独占占用、音频服务不可用或设备失效，应明确返回失败/不支持，不退回墙钟伪装成 native clock。[S1][S3][S6][S7]

## 模块与时间映射设计

| 文件 / 模块 | 职责 |
| --- | --- |
| `src-tauri/examples/support/wasapi_output.rs` | 线程绑定的 shared 静音输出、设备格式/缓冲信息、Start/Stop/Reset、原始 clock 采样和资源释放 |
| `src-tauri/examples/support/presentation_clock.rs` | 不读取墙钟的纯有理数映射，校验频率/回退/溢出，以 epoch 隔离旧样本；合成 PTS scheduler |
| `src-tauri/examples/native_clock_probe.rs` | 生命周期序列、有限运行、采样、判定与 JSON 报告；没有 Tauri 窗口或视频 renderer |
| `src-tauri/Cargo.toml` | `native-clock-probe` 非默认 feature 与 required-feature example，Windows API features 仅显式探针启用 |

项目时间以设备位置差表示：

```text
elapsedSeconds = (currentDevicePosition - anchorDevicePosition) / deviceFrequency
projectFrameExact = anchorProjectFrame + elapsedSeconds × fpsNumerator / fpsDenominator
```

实现使用检查过的 u128 中间乘法及有理数余数，公开整数帧时才取整；不能每轮累加浮点 delta。设备位置减少、零频率、频率变化、无锚点或算术溢出应显式拒绝。Reset 后 raw position=0是合法新起点，必须新建epoch并重新锚定；旧epoch样本即使数值看起来有效也不能进入新项目位置。

Stop/Resume 不等于新项目 seek：只暂停时保留锚点与epoch，从设备冻结位置继续。探针的“seek”只验证 Reset 后映射到项目第900帧，并拒绝旧epoch样本；**没有执行真实媒体解码器 seek**。

合成视频账本只选取设备时钟对应的到期30fps PTS：重复观察同一帧与轮询间隔跳过的PTS须分别计数，不能把它们称为GPU显示重复帧/丢帧。实际视频要共享这个时基仍需要下一阶段接入解码PTS、D3D呈现与可测的输出延迟。

## 运行序列与判定

1. 三次独立打开/释放循环，每次先确认新流 position=0、读回格式/clock frequency，再预填静音。
2. Start 后采样约1500ms；Stop 后间隔250ms读两次位置，要求 position冻结。
3. 预填并 Resume 后采样约1000ms；要求继续旧位置且不回退。
4. Stop→Reset，确认位置为0；重新锚定项目第900帧并换epoch，验证旧epoch被拒绝，再运行约1000ms。
5. 另开独立流执行默认20秒连续采样（可配置2–60秒），随后 Stop250ms冻结检查、Reset和资源释放。

填充每约2ms检查可用容量，clock每约10ms采样；二者是轮询目标而非Windows硬实时保证。日志记录实际最大轮询间隔，不能将定时器目标当作观测值。

有效样本记录原始 position/frequency/qpc100ns/HRESULT/reliability、host monotonic采样时刻、epoch、项目整数帧/小数余数和合成PTS选择。大整数位置与u128字段以字符串保存，避免 JSON→JavaScript 超过安全整数精度。

当前短探针健康判据包括：启动/连续时钟有前进、同epoch不回退、Stop冻结、Reset归零、重新锚定正确、旧epoch拒绝，以及有效采样数量足够。设备时间与host wall推进率0.85–1.15、设备时间与配对QPC历时差小于100ms属于粗粒度健康检查；**不代表一帧音画同步或专业播放质量达标**。对初始可靠读回最多尝试8次，每次S_FALSE均保留原始样本；运行中的S_FALSE同样保留，不让无期限重试掩盖异常。

为了避免末尾失去可靠时钟却仍报告成功，连续阶段要求最近一次可靠采样距当前不超过250ms；每轮及阶段结束都检查覆盖。先记录原始读回再校验回退/异常，失败样本也保留，并附 `validationError`；checkpoint有限重试中的不可靠样本纳入总计。报告使用无覆盖发布，文件名冲突时保留已有文件和新的证据，不能靠重写旧报告“修复”失败。这些行为另有不打开音频设备的回归测试。

## 运行方法与产物

在仓库根目录可复现：

```powershell
cargo test --locked --manifest-path src-tauri/Cargo.toml --features native-clock-probe --example native_clock_probe
cargo run --locked --manifest-path src-tauri/Cargo.toml --features native-clock-probe --example native_clock_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-clock-validation' --seconds 20
```

根包同时提供等价别名：

```powershell
pnpm native:clock-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-clock-validation' --seconds 20
```

也可在 `src-tauri` 目录省略 `--manifest-path`。`--seconds` 默认20，范围2–60；整体执行还包含上述三次生命周期循环，因此总耗时长于该连续采样参数。输出目录必须为本仓库 `src-tauri/target` 内的绝对路径，解析链接后仍在目标目录内。每次新建 `run-<UUID>/`，保存 `native-clock-validation.json`，不覆盖已有报告。

测试分两层：纯映射/状态边界测试不证明设备可用；显式运行example才尝试真实shared render端点。若初始化或设备读回失败，报告原始失败原因和已取得的部分阶段，不改系统设置让它“通过”。正常桌面应用不启动该探针。

## 本机实测（2026-10-16）

原始报告：[native-clock-validation.json](../src-tauri/target/native-clock-validation/run-b792c56f-4921-432c-accb-a89338c14fae/native-clock-validation.json)。报告 `ok=true`，全部原始采样与checkpoint共2901条，未观察到S_FALSE。本轮未执行第二次设备长测来扩展这个单机短样本的结论。

环境：Windows 11 Home x64，版本10.0.26300。独立程序 `src-tauri/target/debug/examples/native_clock_probe.exe` 的SHA-256为：

```text
0076083E9F0D2DB409063D35236F75CC9F53A009BAB116D4DE34B4A60215FBF1
```

| 设备读回 | 实测值 | 解释 |
| --- | --- | --- |
| Mix format | 48000Hz、2声道、32bit、formatTag65534、blockAlign8 | 65534表示扩展格式标签；不据基础头字段独自推断完整subformat |
| Buffer | 4800音频帧，请求100ms | 这里恰好对应48kHz下100ms，不替其它设备作保证 |
| Clock frequency | 384000单位/秒 | 与48000Hz采样率不同；本机数值恰为sampleRate×blockAlign，仍使用API返回frequency换算 |
| Device period | 默认10ms、最小3ms | 实际API读回，不等于本程序轮询周期 |
| Stream latency | API报告0个100ns | 不代表扬声器/声卡端到端物理延迟为0 |

三次独立生命周期循环均执行Start约1500ms→Stop250ms→Resume约1000ms→Stop/Reset→rebase第900帧→再运行约1000ms。三次Stop前后位置均为 `579840`，250ms等待期间保持不变；Resume保留原epoch从旧位置前进。Reset后三次位置均为0，新epoch=2、旧epoch=1；新项目帧=900，旧epoch观察被拒绝。最后20秒连续阶段的Stop读回也冻结：前后均为 `7683840`。

| 20秒连续阶段 | 实测 |
| --- | ---: |
| S_OK有效样本 | 1887 |
| 设备位置对应历时 | 19.981604s |
| Host单调时钟历时 | 19.994359s |
| 相关QPC历时 | 19.994358s |
| 设备/Host推进比 | 0.999362 |
| 最大观察轮询间隔 | 13.128ms |
| 结束时最近可靠样本年龄 | 7.327ms |
| 首次观察到位置前进 | 22.328ms |
| 合成PTS选择 / 跳过 | 600 / 0 |

22.328ms从 **Start返回并取得首次S_OK后的采样循环起点** 计时，不是精确Start调用到设备输出的启动延迟。该阶段共1887次有效观察，选择600个合成30fps PTS，其余为重复观察；“零跳过”只描述账本，不是已解码视频的显示FPS/丢帧结果。设备与QPC约12.75ms的累计历时差通过本探针100ms健康门槛，不能用于宣称音视频已小于一帧。

`resourcesReleased=true` 的证据是显式Stop/Reset、退出各Rust资源作用域及程序正常结束；没有进行系统级句柄/COM泄漏跟踪，因此不能称为泄漏证明。没有启动主播放器或Tauri窗口，没有录音或修改系统音频设置。

本轮普通Rust回归100项通过、6项既有设备相关用例默认忽略；前端231项、lint、前端build及cargo fmt检查通过。探针独立测试统一复测17项通过，覆盖时间映射/epoch、buffer状态、HRESULT精度分类、250ms可靠覆盖、失败原始样本保留和报告冲突保全。未重新制作Tauri安装包，因为变化位于显式feature下的独立example，主应用播放路径未替换。

本轮修改文件：`src-tauri/Cargo.toml`（非默认feature及example入口）、新增`src-tauri/examples/native_clock_probe.rs`、`examples/support/wasapi_output.rs`、`examples/support/presentation_clock.rs`；`package.json`增加显式运行入口；README、架构、任务、原生命令契约和本文同步范围。没有新增依赖版本或更换FFmpeg，Cargo.lock未因本探针更新。原始报告目录还保留`build-context.json`，记录程序与本轮源码哈希，便于复核。

## 下一步与仍未解决的问题

下一探针应接入真实媒体解码所得PCM样本和视频PTS，将PCM写入WASAPI并验证源区间/暂停/恢复/seek，再接D3D实际呈现和测量。共享时钟的选择账本只有在真实媒体队列与呈现器都按同一时基工作后，才能进一步评价启动音画同步。

当前WebView暖启动约49–56ms时差仍是已记录限制。本次独立静音时钟成功没有修复它，也没有验证多音轨混音、丢帧策略、VFR/HDR、设备切换、长时音质或可见窗口表现。

## 官方来源

- [S1] [IAudioClock::GetPosition](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getposition)：position单位、关联QPC的100ns单位、S_FALSE、启动/停止/重置语义。
- [S2] [IAudioClock::GetFrequency](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getfrequency)：兼容位置单位的频率与drift/jitter范围。
- [S3] [IAudioClient::Start](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-start)：预填充与恢复。
- [S4] [IAudioClient::Stop](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-stop)：停止冻结与S_FALSE。
- [S5] [IAudioClient::Reset](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-reset)：stopped/no pending buffer要求与归零。
- [S6] [IAudioClient::GetMixFormat](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat)：共享模式格式、结构所有权与释放。
- [S7] [IAudioClient::Initialize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize)：共享模式period=0、duration单位与初始化要求。
- [S8] [IAudioClient::GetCurrentPadding](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getcurrentpadding)：padding帧数与可写容量。
- [S9] [IAudioRenderClient::ReleaseBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-releasebuffer)：SILENT标志和缓冲配对。
- [S10] [AUDCLNT_STREAMFLAGS constants](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants)：NOPERSIST会话设置范围。
- [S11] [QueryPerformanceFrequency](https://learn.microsoft.com/en-us/windows/win32/api/profileapi/nf-profileapi-queryperformancefrequency)：counts/sec频率。
- [S12] [QueryPerformanceCounter](https://learn.microsoft.com/en-us/windows/win32/api/profileapi/nf-profileapi-queryperformancecounter)：原始计数单位。
- [S13] [IAudioRenderClient::GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-getbuffer)：同线程与配对顺序。
- [S14] [Rendering a Stream](https://learn.microsoft.com/en-us/windows/win32/coreaudio/rendering-a-stream)：Microsoft shared-mode输出示例。
- [L1] 本机 Cargo registry 的 `windows-0.62.2/src/Windows/Win32/Media/Audio/mod.rs` 第2935–2958行：`IAudioClock`便捷方法与raw vtable；复核版本时注意生成绑定可能变化。
