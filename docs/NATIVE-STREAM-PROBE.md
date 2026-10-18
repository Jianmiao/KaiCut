# Windows 原生流式解码探针

**状态：受控流式功能验证通过，实时性能尚未达标。** 本轮从固定FFmpeg/ffprobe管道逐块读取PCM/RGB与真实PTS，完成4秒整流及20秒源的部分播放、顺序重启seek和自有进程回收。最终报告明确`functionalPassed=true`、`realtimePerformanceQualified=false`；首轮超过音频缓冲预算的供给间隔和多秒GPU回收异常仍保留，不由后续较好数据覆盖。

入口为非默认feature `native-stream-probe`下的独立example `native_stream_probe`。正常KaiCut播放器、UI、项目文件、MCP工具与发布包不切换到该路径。没有Present、可见窗口、桌面控制、capture/loopback录音或可听测试音；实际PCM仍进入自有静音WASAPI session，渲染使用已有离屏异步worker。

## 本轮变化与沿用范围

| 部分 | 本轮范围 |
| --- | --- |
| 解码器 | 复用本仓库既有LGPL-2.1-or-later FFmpeg 7.1/ffprobe；固定参数、file输入，未下载、重编译或增加codec |
| 音频 | `pcm_s16le`编码到WAV pipe，增量解析为完整PCM帧；不依赖当前构建未提供的`s16le`裸流muxer |
| 视频 | 160×90 RGB rawvideo管道，按完整帧读取；独立ffprobe compact管道逐条提供同一流的真实`best_effort_timestamp` |
| 时间戳 | 应用侧只先读取有界stream元数据/time_base，逐条消费PTS、不保存整段PTS表；ffprobe可能已经把metadata写入OS pipe后退出，不能声称该子进程没有提前完整分析 |
| 解码队列 | 最多8块音频，每块480音频帧；最多4个视频帧，有界背压 |
| 音频输出 | 本验收driver明确限制48kHz双声道端点；主线程非阻塞获取已准备块并提交实际PCM，WASAPI position/frequency仍为时钟来源 |
| 渲染 | 沿用pending2/in-flight1/latest completion1的异步离屏队列，GPU工作不在解码队列锁内 |
| seek | 停止并重启本探针自有decoder，从文件起点顺序解码/丢弃到精确源位置；不是长GOP快速seek实现 |
| 状态 | 纯session策略记录Preparing/Ready/Playing/Paused/Ended/Cancelled/Closed/Failed及epoch，真实IO观测由driver提供 |

解码器内部缓存、操作系统pipe缓冲、单个reader当前持有的块、driver的半块PCM、下一视频帧及保留待重试的当前视频帧、WASAPI endpoint缓冲和渲染队列是不同层，不能只报queue高水位就声称总内存精确等于8×480＋4帧。最终driver另外保留最多1个`currentVideo`用于Busy时重试，不无界积累。应用持有集合与诊断trace有明确上限，不随媒体时长保存全文。

sidecar来源为FFmpeg上游tag`n7.1`、commit`b08d7969c550a804a59511c7b83f2dd8cc0499b8`，许可与固定二进制哈希见 [binaries/README.md](../src-tauri/binaries/README.md) 和 [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)。本轮只复用这些文件，没有扩大编解码器或下载来源。

## WAV pipe的增量契约

当前sidecar使用PCM16 WAV pipe，不能复用“完整RIFF长度必须等于整个文件字节数”的离线解析条件。不可seek的输出可能用RIFF/data未知长度标记（例如`0xFFFFFFFF`）；必须针对实际管道输出明确支持这些标记，不把任意巨大chunk长度作为无限内存分配依据。

- 增量读取RIFF/WAVE标识、chunk header、fmt及必要扩展；限制头部扫描和可跳过chunk字节数，正确处理奇数chunk padding。
- 检查PCM16类型、实际输出sampleRate/channels/blockAlign及fmt长度；未知或矛盾格式直接失败，不猜测设备编码。
- data只按完整音频帧交付，480帧为通常块大小；最终块可短于480，但必须含完整声道帧。EOF留下半个i16、残缺多声道frame或未完成chunk header，属于截断，不得当正常EOF。
- 未知data长度下，pipe EOF只表示读流结束；还要等待对应FFmpeg退出并检查成功，不能把错误退出或主动kill产生的EOF当成正常源结束。
- 已知data长度必须核对实际读入数量；超过/少于声明范围不能静默接受。读取与转换使用固定小缓冲，不收集整个WAV再解析。

PCM队列取出一块后，WASAPI本次可能只接受其中一部分。driver必须保留块内消费offset并在后续继续，不能pop后丢弃尾样本；完整源hash按**真正提交的样本序列**计算。

## RGB与真实PTS的一一对应

RGB解码和ffprobe都选择同一个视频流；沿用无fps复制/丢帧的passthrough输出。每次读取完整`160×90×3`字节RGB帧，并取得一个有效decoded frame PTS后组成队列项，保留帧序号、原始timestamp、time_base、归零后的ns与RGB hash。

compact输出不是可无条件按整行整数解析的私有协议；首帧可能带side-data字段或分隔空行。应按本轮实际观察的key和行结构识别frame条目，对合法附加字段作显式处理，对缺少PTS、重复字段、无效数值、超长行、倒退/重复PTS和截断行报错。不能将所有“解析不了”的行跳过，从而让RGB帧错位后仍继续。

一侧EOF时需检查另一侧也没有剩余帧/时间戳；只有RGB帧完整、PTS数量/顺序一致，以及video FFmpeg和PTS ffprobe均干净退出，才能标记video source EOF。两条管道并行生产、有界交替消费；不能先排空整条PTS管道再读RGB，也不能在持有共享queue锁时阻塞读pipe。

time_base换算与负原点继续使用检查过的宽整数。首PTS归零、音视频源起点关系仅按受控夹具约定，不扩展成任意容器edit list、音轨偏移或VFR兼容声明。

## 启动、暂停与结束

启动门槛是实际音频prefill已填满本次4800帧WASAPI缓冲，并且取得当前epoch首个可用视频帧；不是仅创建进程或把纯session置Ready。输入音频queue最多3840帧，driver边取块边填endpoint。本验收只支持明确4秒/20秒夹具；短于endpoint缓冲的源尚无不足prefill就绪策略，可能准备超时，不能宣称任意短源可用，也不补假音频凑长度。

pause执行Stop并确认设备位置冻结，保留尚未消费的解码块、当前半块offset及已排队PCM；renderer epoch失效。暂停期间consumer不再推进，reader及子进程可因有界队列/pipe自然背压；这种无输出不是decoder故障，不应用运行中的无前进计时器将主动暂停判成hang。

结束必须区分以下事件：

1. 音频decoder实际干净EOF及成功退出。
2. 视频RGB与PTS配对完毕，video decoder和ffprobe均成功退出。
3. 所有真实音频帧已提交，当前半块和音频queue已耗尽。
4. WASAPI设备时钟到达真实源末尾且padding=0。
5. 当前epoch的最后源视频帧已取得真实GPU验证完成；4秒夹具要求index119，不能仅凭视频decoder EOF结束。

只有联合条件满足，driver才向纯session策略报告Ended。临时queue空、某个reader读取到EOF、某个进程退出或状态机单独接受事件都不是整个播放完成的证明。实际音频结束判据仍不是物理扬声器末采样的听感测量。

## 精确重启seek与epoch

本轮1.5秒seek为顺序重启：停止当前输出，推进session/render epoch，使旧回调和完成结果失效；清理旧半块/队列、终止并回收旧decoder，再启动新的有界管道，从头顺序丢弃到目标位置。

音频offset按**设备格式重采样后**的sample rate计算，例如48kHz时1.5秒是72000音频帧，不按clockFrequency计算。跳过过程仍采用小块读取/丢弃，不先把目标前全部样本存起来。首个交付块如跨过目标，裁掉前缀后保留准确余下内容，最终提交hash应匹配离线参考的对应源切片。

视频首帧应是最后一个`PTS <= target`的帧；如果目标不恰好落在帧边界，应保留覆盖该时间的前一帧，不只取第一个大于目标的PTS。相应时间映射以原始源PTS为准，不通过更换帧号掩盖源偏移。该策略的seek时间随目标位置增长，是本轮已声明的限制，不叫“快速原生seek”。

纯`preview_session`仅决定合法转换、epoch及结束条件；它不调用FFmpeg、WASAPI或D3D，也不证明它们已经停止。driver必须在IO实际成功后通知策略；旧epoch的ready/EOF/error回调不能复活已seek或取消的状态，Failed/Cancelled/Closed为终态，后续需要新会话而非偷偷resume。

## 自有进程、reader与错误收尾

本轮流式播放涉及三个长驻自有子进程：audio FFmpeg、video FFmpeg、PTS ffprobe；有界stream metadata查询可使用短命辅助进程，分别记录其实际生命周期。只通过创建时持有的Child/Job句柄操作这些进程，不枚举并按名称结束其它FFmpeg。

- 管道stderr必须持续排空且只保留固定上限尾部，否则stderr写满会让正常decoder停住；截断标记/错误退出原因进入报告。
- reader在queue满时用可取消等待；等待期间释放queue锁。主线程poll使用try_lock，遇忙返回暂不可用，不通过新隐藏的阻塞health/stat路径绕过非阻塞契约。
- seek/cancel先设置取消标记并notify，让queue背压等待解除；随后kill/wait本次三个子进程，使正在read/read_exact的pipe reader看到关闭；最后join reader。先join再kill可能永久等待pipe输入。
- 正常EOF要检查exit status=0；主动取消的kill退出是Cancelled清理，不是成功EOF。进程启动失败、某一reader报错或半帧截断时，剩余自有进程也要收尾，不能只停止报错的那一个。
- Job Object使用kill-on-owner-close保护自有树；成功分配之后才有这项保护。若采用普通spawn后再Assign，须准确记录其短启动窗口，不能宣称原子suspended创建。分配失败立即kill/wait自有child，不降级成无保护继续。
- 任意失败先Stop/Reset或释放自有音频，再等待图形worker/decoder收尾，避免GPU异常等待期间留下Running但无人供给的音频流。

pipe截止时间、取消通知和Job Object不等于Rust能安全强杀任意reader线程；join成功要实际确认，不能把发出kill或close意图当成已回收。不能保证驱动永久阻塞有可强制的线程级退出时限，保留前轮文档中的限制。

## 本轮验收矩阵

### 4秒完整流与1.5秒seek

使用前轮生成的4秒MP4/WAV，以及对应离线解码的`decoded-audio.wav`、`decoded-video.rgb`作只读参考：

- 整流PCM实际消费序列与参考完全一致，音频总frame_count、每声道顺序、非零样本及hash核对。
- RGB逐帧数量、内容hash与PTS映射对应；不能因只有queue中4帧就忽略后续内容的完整性。
- 1.5秒重启seek后首PCM offset、首视频PTS及后续消费切片与参考一致；旧epoch样本拒绝。
- pause保留已缓冲内容，resume没有重放/丢失半块；EOF联合条件、取消Stop/Reset、三个进程退出和reader join均有实际证据。

### 20秒受控流与背压

另用显式生成的20秒受控夹具验证应用侧PCM/RGB逐步读取和背压。实际只播放0→2秒，再重启seek至10秒并播放到约11秒后取消，**不是20秒连续播放或20秒整源EOF验收**。探针本身不隐式生成或下载素材、不扩codec：

- Start时记录应用reader读出的音频/RGB数量、EOF和进程状态，确认没有在应用内预先完整缓存PCM/RGB。PTS ffprobe在前两轮prepare时已经exit0，metadata可能已全量写入OS pipe；不能据reader计数推断所有子进程都未完整分析源。
- 消费变慢或暂停时，音频queue高水位≤8块，每块≤480帧；video queue高水位≤4帧；renderer仍遵守既有2+1+1约束。统计应用缓存与reader当前块，不把OS pipe/decoder内部缓存算成0。
- 不因背压等待放宽失败条件；中途截断或decoder非零退出即使已有大量有效内容，也不能以已消费部分作为整流成功。
- 源文件hash/大小/mtime保持不变，输出报告使用新目录/无覆盖发布，状态采样与stderr存储有界。

### 确定性失败用例

纯解析与线程测试覆盖未知WAV长度、残缺header/样本帧、错误fmt、RGB部分帧、缺失/多余/非单调PTS、reader queue满后取消、部分进程启动失败、非零退出与错误传播、旧epoch回调、源EOF但设备未drain等。测试中的受控fake reader/process只用于准确触发边界，不能替代上面真实FFmpeg/WASAPI/D3D设备运行。

最终结果按功能、供给预算和实时性能分别判定；功能成功不能抵消前两轮记录的调度和回收异常。

## 运行方法

本轮独立入口：

```powershell
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-stream-validation' --long-fixture-dir '<已生成20秒夹具目录>'
```

显式指定全部输入：

```powershell
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-stream-validation' --fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\workflow-validation\98ee1285-41b7-452a-bbce-e38c83b1a2aa' --reference-dir 'E:\KaiCut（开剪）\src-tauri\target\native-d3d-validation\run-ec927ae2-fcfe-4e7c-8ce0-475746a10284' --long-fixture-dir '<已生成20秒夹具目录>'
```

默认4秒fixture目录沿用上述workflow路径，默认reference目录为上述旧D3D运行，需存在`decoded-audio.wav`和`decoded-video.rgb`。`--long-fixture-dir`本轮必填，不隐式生成长素材。`--output-dir`必须为本仓库`src-tauri/target`内绝对目录，每次创建新运行子目录；不覆盖此前验证结果。

别名对应：

```powershell
cargo run --locked --manifest-path src-tauri/Cargo.toml --features native-stream-probe --example native_stream_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-stream-validation' --long-fixture-dir '<已生成20秒夹具目录>'
```

报告列各case的Start前reader计数/EOF、queue高水位、PCM/RGB/PTS内容验证、半块消费、供给/采样间隔、纯session状态与真实IO事件、自有PID退出/reader join及源hash。不存在的物理输出证据不由状态机补成成功。

## 三轮真实运行与未关闭的质量问题

三份原始报告保留各自当时的数据与判定，早期`ok=true`只代表其当时的功能条件，不等于实时性能通过：

- [初测](../src-tauri/target/native-stream-validation/run-ee0ed9d4-1a75-49f6-a631-b096ca9221ea/native-stream-validation.json)：没有分开的实时质量字段；出现154.3662ms供给间隔和2754.2575ms图形回收。
- [分段计时复测](../src-tauri/target/native-stream-validation/run-18210ffe-8f29-4970-9c86-2631a226caae/native-stream-validation.json)：加入供给、sleep、stats与图形回收前状态观测，仍存在明显抖动。
- [最终功能报告](../src-tauri/target/native-stream-validation/run-5f1da80e-6826-4c4f-b0fa-ef4065e25762/native-stream-validation.json)：收紧末帧GPU完成、Busy重试、真实PTS范围、原始时钟记录和失败清理，`functionalPassed=true`且`realtimePerformanceQualified=false`。

最终独立汇总：[verification-summary.json](../src-tauri/target/native-stream-validation/run-5f1da80e-6826-4c4f-b0fa-ef4065e25762/verification-summary.json)。实际运行代码/程序身份：[build-context.json](../src-tauri/target/native-stream-validation/run-5f1da80e-6826-4c4f-b0fa-ef4065e25762/build-context.json)，程序SHA-256为`30f97b858b24f73b912fa731e19f6a85484d831bcedc6337ffe4f90fc02b8387`。早期两轮的原可执行文件未归档，不能用最终程序哈希替它们作版本证明。最终设备运行后仅格式化重排wrapper模块声明，未改行为；对应差异由交付记录说明，不假称格式后另跑了一次设备。

### 三轮耗时对照

| 指标 | 初测短 / 长源case | 分段复测短 / 长源case | 最终短 / 长源case |
| --- | ---: | ---: | ---: |
| 最大PCM供给调用间隔 | 81.6826 / **154.3662ms** | 60.6636 / 39.3018ms | 42.2363 / 32.3447ms |
| session创建＋prepare | 未测 / 未测 | 2622.3465 / 4561.7771ms | 2990.4738 / 704.7365ms |
| prepare单独计时 | 443.5208 / 1714.7344ms | 367.1423 / 1436.7439ms | 1522.0461 / 66.6133ms |
| seek重新prepare | 1390.9327 / 2019.9721ms | 1371.7473 / 658.1451ms | 115.3442 / 440.3160ms |
| renderer shutdown | **2754.2575** / 55.7067ms | **305.8666 / 284.9851ms** | 41.8351 / 107.1533ms |

本机endpoint为48kHz、4800帧，即100ms缓冲。首轮154.3662ms供给间隔已超出这个预算；虽然轮询`underrunObservations=0`，仍不能声称无glitch或稳定供给。第二轮六阶段间隔约30.88–60.66ms，虽然低于100ms，也不是2ms级稳定唤醒。

第二轮请求sleep2ms，实际最长57.6779ms；同时测得stats最长5.8187ms、pump15.0192ms、视频分发23.6675ms、renderer poll0.2032ms。这支持“CPU调度/分段工作有明显耗时”的观察，但没有足够证据把全部抖动唯一归因于sleep。第二轮两个case在renderer shutdown前都已`inFlight=false`，仍耗时约306/285ms；首轮2.75秒回收原因未被最终较快一次根治。

创建＋prepare是调试诊断开销，prepare计时不含之前设备/renderer/decoder创建。不能只取最终长case的66.6ms就称原生启动已快，也不能把这些数字称为首声音/首显示延迟。三轮样本有限且实现观测/门槛在变化，不作统一百分比性能提升结论。

### 最终功能与供给预算结果

| 最终结果 | 4秒完整源case | 20秒源部分播放case |
| --- | --- | --- |
| 应用reader在首Start前读取 | 9120音频帧＋7RGB帧 | 9120音频帧＋7RGB帧 |
| WASAPI prefill | 4800音频帧 | 4800音频帧 |
| 音频 / 视频queue高水位 | 8块 / 4帧 | 8块 / 4帧 |
| 主线程消费verified render | 96条 | 89条 |
| 原始观察记录 | 673条 | 489条 |
| 最长请求2ms的实际sleep | 39.9517ms | 31.5116ms |
| feed gap相对100ms缓冲门槛 | 本次通过 | 本次通过 |
| decoder关闭＋reader join | 7.9933ms，全部自有进程已回收 | 11.2651ms，全部自有进程已回收 |
| source / PCM / seek hash | 核对通过 | 已消费区间核对通过 |

短case完整提交192000音频帧，hash=`66be5f80f8dcdcdf4164a2030a788f356ceb62d216fd2eacf545104f8cfcee3e`，解码120个RGB帧；结束要求三个源管道正常EOF、源PCM全部提交、device clock到4秒、padding0且视频末帧119已验证完成。96条消费包含重新seek后的结果，并不等于全源120帧都被显示；本轮完全没有显示输出。

短seek源入点1.5秒对应音频72000帧、视频45；长源seek10秒对应音频480000帧、视频300。两者重启decoder顺序丢弃前缀，后续实际提交切片hash正确；长case只播放初段0→2秒和seek后的10→约11秒，不等待20秒源EOF。两个case暂停实际观察200ms，音频位置冻结；取消后clock/padding0，旧epoch拒绝，记录的12个自有decoder PID及probe结束后均无残留。

首次Start的9120/7计数说明应用reader没有完整缓存PCM/RGB；前两轮两个case的PTS ffprobe在prepare时都已exit0，最终短case亦然。ffprobe可能已将整份metadata写进OS pipe，其内部解码/分析进度不受应用视频queue计数代表。结论仅为应用侧有界逐步读取，不能说整个链路任何进程都没有提前全量分析。

最终添加以下防假通过条件：每阶段消费结果的epoch/index/实际30fps夹具PTS及RGB hash正确；保留当前视频帧以便Busy重试（额外最多1帧）；EOF必须等待末帧119完成；S_FALSE、pause/seek/cancel读回先记录原始状态；失败先停止音频并显式close decoder/renderer，记录清理结果后再复制trace；报告以临时文件无覆盖发布，碰撞保留证据。

### 验证与限制

最终example测试63项通过、1项真实sidecar用例默认忽略；该用例另行显式执行通过，包含4秒完整PCM/RGB、非块对齐72001帧seek、非帧对齐1.500020833秒仍选视频45、20秒满队列取消及bad WAV错误回收。它是固定sidecar集成验证，不能冒充声卡/屏幕实测。默认Rust回归100项通过、6项既有环境用例忽略；前端231项、lint与前端build通过。格式检查最初发现wrapper模块排序问题，格式化后重跑通过，最终diff检查通过。命令结果见 [final-checks.json](../src-tauri/target/native-stream-validation/run-5f1da80e-6826-4c4f-b0fa-ef4065e25762/final-checks.json)。

最后两个case`feedGapsWithinEndpointBudget=true`只说明本次供给间隔未超过100ms；总体`realtimePerformanceQualified=false`仍保持。前两轮异常、秒级创建/准备波动和图形回收抖动尚未根治。主应用未替换，没有Present/可见窗口、录音或可听验证，也不生成新的Tauri安装包来暗示集成完成。

## 下一步与证据限制

下一步优先验证event-driven WASAPI/MMCSS调度及常驻decoder的快速seek，分解session创建和GPU回收延迟；不能通过固定magic sleep或加大缓冲掩盖实时问题，再声称可以切换主应用。

本轮从整段内存解码推进到有界顺序流与可回收生命周期，仍限制48kHz双声道端点、受控MP4/WAV、160×90离屏输出和自有session静音。尚未验证快速随机seek、长GOP/VFR/编码器delay、任意音轨偏移、重采样矩阵、多轨混音或可见swap chain呈现，不宣布暖启动时差已解决。

依据为仓库已有[音频时钟](./NATIVE-CLOCK-PROBE.md)、[真实PCM/PTS](./NATIVE-MEDIA-PROBE.md)、[D3D离屏](./NATIVE-D3D-PROBE.md)、[异步队列](./NATIVE-QUEUE-PROBE.md)的API核对与验证边界，以及本轮固定sidecar实际管道输出/日志。本文不新增未经实测的codec或格式支持承诺，也不将纯session测试等同于设备运行。
