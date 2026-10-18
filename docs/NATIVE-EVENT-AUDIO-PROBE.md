# Windows 事件驱动音频与MMCSS探针

**状态：同一程序的poll、event、event-mmcss及一次MMCSS重复运行均通过功能与本次供给预算检查；总体实时质量仍未关闭。** 本轮观察到事件路径的供给间隔更稳定，但事件缓冲与poll不同，样本也仅限短时单设备。正常应用播放器、UI与项目格式不变，没有Present、可见窗口、桌面控制或录音；真实PCM仍只进入探针自有的静音音频session。

上一轮首测154.3662ms供给间隔超过100ms endpoint缓冲，图形回收曾达2754.2575ms；分段复测请求sleep2ms却观察到最长57.6779ms，且worker已无in-flight时仍有约306/285ms回收。本轮保留这些记录，不因采用事件/MMCSS就预先宣称实时问题解决。GPU回收计时由独立观测拆分，不把它直接归因于音频等待方式。

## 首要API结论：shared事件模式不能沿用100ms请求

Microsoft `IAudioClient::Initialize`官方备注明确：

> For a shared-mode stream that uses event-driven buffering, the caller must set both hnsPeriodicity and hnsBufferDuration to 0.

因此三种模式的设置必须分别记录：[S1]

| 模式 | ShareMode / flags | 请求buffer duration / periodicity | 线程调度 |
| --- | --- | --- | --- |
| `poll` | SHARED、已有NOPERSIST | 100ms / 0 | 现有sleep2ms轮询，不注册MMCSS |
| `event` | SHARED、EVENTCALLBACK＋NOPERSIST | **0 / 0** | 等待音频事件，不注册MMCSS |
| `event-mmcss` | 与event相同 | **0 / 0** | 供给线程每个播放阶段注册MMCSS `Audio`、相对优先级NORMAL |

`GetBufferSize`才是实际帧容量；duration0让音频引擎选择容量，不能在文档或报表中硬写事件模式也是4800帧/100ms。每次保存sampleRate、bufferFrames和换算出的实际bufferDuration，与供给gap比较。即使三组使用相同端点、素材和二进制，buffer请求不同仍是比较变量；不能把所有变化都归因于MMCSS或事件通知。

## 事件句柄与处理顺序

1. 创建未命名、不可继承、初始nonsignaled的auto-reset event。`CreateEventW`的manual-reset参数为FALSE、initial-state参数为FALSE；不要复用有未知状态的命名系统事件。[S3]
2. 用EVENTCALLBACK初始化shared流，duration和periodicity都为0；成功后、Start之前调用`SetEventHandle`。Start时事件应为nonsignaled；没有启用EVENTCALLBACK却设置事件，或未设置事件就Start，都不能当成合法降级路径。[S1][S2]
3. 与之前相同，先写入真实PCM prefill再Start。每次事件唤醒后读取`GetCurrentPadding`，按实际可写空间提交完整音频帧。一个通知不保证固定帧数，也不保证所有待写空间都需要用一个包填满。
4. 阶段内使用最多25ms等待；记录signaled、timed out、失败和实际等待耗时。超时明确报错并停止本次case，**不悄悄退回轮询**。25ms是请求的等待上限，不是线程一定在25ms内被调度的保证。
5. Stop/Reset/Resume按原生命周期执行；Resume的Start之前可以清理旧的残留信号。流运行期间不能每轮手工ResetEvent，否则可能吞掉刚产生的有效通知。句柄由本输出对象持有，不能在仍等待/仍被流使用时关闭。

auto-reset只在释放一个等待线程后自动清为nonsignaled；没有等待者时会保留signaled。它不是事件次数计数器，因此driver必须以padding为准，不从收到的事件个数计算已播放样本。[S2][S3]

如果设备实际引擎周期或系统调度导致25ms等待无法满足，应保存失败并说明此探针等待约束，不修改系统设备设置以“通过”。更小的实际buffer意味着更紧供给预算；只看到最大gap低于旧100ms不能作为新事件模式通过依据。

## MMCSS作用范围与恢复

`AvSetMmThreadCharacteristicsW`关联**调用线程**与指定任务。`Audio`是Microsoft列出的默认任务之一；TaskIndex首次传0，成功时记录API返回的index和任务handle。`AvSetMmThreadPriority(..., AVRT_PRIORITY_NORMAL)`将优先级设为同任务内的正常相对档位，不等于固定Win32线程优先级，也不保证硬实时截止。[S4][S5][S7]

本轮只在实际执行WASAPI供给的线程、每个play/resume/seek播放阶段进入MMCSS；设备/decoder创建、参考文件哈希、GPU清理等不借用该调度身份。不开`Pro Audio`/CRITICAL，不改进程优先级、affinity、timer resolution、系统registry或其它线程。

阶段成功和失败路径都显式调用`AvRevertMmThreadCharacteristics`并检查返回，报告`registered/active/reverted/taskIndex/revertError`。该API必须由创建handle的同一线程调用；用`!Send`作用域guard限制线程迁移，Drop只作错误路径兜底。恢复失败不能记录`reverted=true`；保留handle以便兜底重试，并把恢复错误合并进本次失败，而不是被原播放错误覆盖。[S6]

如果注册成功而NORMAL设置失败，也必须先恢复已经注册的任务；注册失败、权限不足、任务名无效或MMCSS服务不可用时明确失败，不自动换成event-only。Avrt任务handle通过AvRevert恢复，不用普通CloseHandle代替。

Microsoft说明MMCSS根据任务profile、相对优先级、前后台状态和CPU使用调整线程优先级；它不是CPU独占或调度延迟为零的保证。官方也建议不要无必要地提高系统全局timer resolution。本轮不调用timeBeginPeriod，也不修改SystemProfile任务配置。[S7]

## 同一二进制的对照方法

固定一次代码/二进制身份，依次执行`poll → event → event-mmcss`，使用相同4秒与20秒受控素材、参考解码结果、设备、图形renderer和管道实现。必要时只追加一次明确标注的重复，保留所有原始运行，不挑最好一次代替失败或抖动。

功能范围沿用前轮：

- 48kHz双声道端点、160×90离屏视频、固定MP4/WAV和既有LGPL-2.1-or-later FFmpeg7.1/ffprobe；没有重建、下载或增加codec。
- 4秒源完整流至EOF、暂停/恢复、1.5秒精确顺序重启seek、取消。
- 20秒源只播放0→2秒及重启seek后的10→约11秒并取消；不是20秒连续播放或长媒体全面支持。
- PCM完整/切片hash、视频index/实际PTS/RGB hash、末帧119验证、三自有decoder回收、reader join、epoch隔离、队列8×480/4帧与renderer2+1+1保持原检查。
- 应用侧未完整缓存PCM/RGB，不宣称PTS ffprobe未提前把整段metadata写入OS pipe。

各模式输出以下独立维度：

| 维度 | 记录与判定 |
| --- | --- |
| 请求与实际格式 | event flag、requested duration、actual bufferFrames/sampleRate、设备period、session-muted读回 |
| 等待 | wait次数、signaled次数、timeout次数、最大实际wait、poll实际sleep；失败保留原系统错误 |
| 供给 | 最大pump gap、单次pump耗时、gap相对**实际**buffer duration、观察下溢次数；0观察下溢不等于物理无glitch |
| 原始时钟 | position/frequency/QPC/HRESULT/reliability、可靠覆盖；事件次数不替代时钟读回 |
| 分段CPU耗时 | decoder stats、视频分发、renderer poll、会话创建、prefill；不把prepare单独时间当总启动延迟 |
| MMCSS | 每阶段是否注册、NORMAL是否成功、是否显式恢复、错误；poll/event不得伪填注册成功 |
| 图形回收 | worker工作结束、backend析构、join等可测区间分别报告；未测的驱动内部时间保持未知 |
| 内容与资源 | PCM/RGB参考验证、EOF联合条件、source hash、自有PID回收和失败清理 |

报告区分`functionalPassed`、供给预算检查和总体实时质量。事件模式能完成PCM内容验证，只证明该功能路径可用；想关闭实时门槛仍需实际gap、等待、回收和重复稳定性证据。也不能仅因事件buffer变小导致某个比值变化，就误判调度方式优劣。

## 运行入口

沿用`native_stream_probe`，新增显式`--audio-scheduling`参数，取值为`poll`、`event`或`event-mmcss`。不存在`auto`或未声明的fallback；未提供参数保持已有poll行为。

```powershell
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-event-audio-validation' --long-fixture-dir '<已生成20秒夹具目录>' --audio-scheduling poll
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-event-audio-validation' --long-fixture-dir '<同一20秒夹具目录>' --audio-scheduling event
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-event-audio-validation' --long-fixture-dir '<同一20秒夹具目录>' --audio-scheduling event-mmcss
```

4秒fixture与reference默认路径、可选`--fixture-dir/--reference-dir`及target内输出限制沿用 [NATIVE-STREAM-PROBE.md](./NATIVE-STREAM-PROBE.md)。每次新run目录，不覆盖旧报告。执行者应在三次运行前固定二进制及源码身份，三组之间不要改变实现；构建/单测失败先修复再重启整个比较。

## 同一程序的本机对照结果

本批依次运行三个模式，再追加一次`event-mmcss`重复，没有在组间更改实现。批次目录为`src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/`，四份原始报告均`ok=true`、`functionalPassed=true`、`realtimePerformanceQualified=false`，每个短/长源case均`feedGapsWithinEndpointBudget=true`：

- [poll报告](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/run-b1eef86f-e1f4-4ec9-b664-35dd0762b81d/native-stream-validation.json)。
- [event报告](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/run-fbc8e6e9-54f9-4dbc-bc92-42be05f977d0/native-stream-validation.json)。
- [event-mmcss报告](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/run-aa65649e-0a28-47cd-b2ed-787a51080c8f/native-stream-validation.json)。
- [event-mmcss重复报告](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/run-dae7413b-2f47-4c37-9f17-347b8dfbba6d/native-stream-validation.json)。

以下“短/长”指4秒源完整播放case和20秒源**部分播放**case；后者仍只播放0→2秒、10→约11秒并取消。

| 模式 | 实际buffer | 最大供给gap：短 / 长 | event signals：短 / 长 | event timeout |
| --- | --- | ---: | ---: | ---: |
| poll | 4800帧 / 100ms | 26.9900 / 19.3907ms | 不适用 | 不适用 |
| event | 1056帧 / 22ms | 12.7166 / 10.3458ms | 504 / 306 | 0 |
| event-mmcss | 1056帧 / 22ms | 10.4474 / 10.4127ms | 504 / 305 | 0 |
| event-mmcss重复 | 1056帧 / 22ms | 10.3755 / 10.3807ms | 505 / 306 | 0 |

sampleRate均48000，channel均2。poll请求100ms；三个event运行请求duration=0并由引擎返回1056帧，实际22ms。表中的事件模式gap都低于本次22ms预算；没有用旧100ms预算放宽判定。事件唤醒周期不能从22ms容量直接推断，必须看实际signal与等待采样。

| 模式 | 最大实际wait：短 / 长 | 最大可靠clock采样gap：短 / 长 | 单次pump最长：短 / 长 |
| --- | ---: | ---: | ---: |
| poll | 26.5113 / 19.3735ms | 42.9622 / 27.4050ms | 12.1365 / 3.7376ms |
| event | 11.5028 / 9.9245ms | 20.7560 / 20.4250ms | 1.6670 / 0.7836ms |
| event-mmcss | 10.1414 / 10.0055ms | 20.3480 / 20.3495ms | 0.5562 / 0.4928ms |
| event-mmcss重复 | 9.9480 / 9.9456ms | 20.3604 / 20.4278ms | 0.6503 / 0.5520ms |

poll的wait是请求sleep2ms的实际经过时间，event是等待音频事件的实际经过时间；二者用途不同，不能只比较越短越好。event与event-mmcss共享22ms容量，较适合观察MMCSS带来的小变化，但各一次及一次重复仍不足以得出跨负载或跨设备因果结论。poll→event同时改变了唤醒方式与实际buffer大小，不能称为纯MMCSS性能收益。

两次MMCSS运行各有六个播放阶段（短/长case的initial、resume、seek），均记录`registered=true`、`priority=normal`、`taskName=Audio`，退出后`reverted=true`、`active=false`、`revertError=null`。poll与event未注册MMCSS，返回对应的false/null状态；没有伪填恢复成功。

所有case的PCM完整/seek区间hash、实际视频PTS/RGB关联、短源最后119帧GPU完成、源文件指纹、pause/seek/cancel及自有进程/reader回收均通过。每阶段观察下溢为0；它仍是轮询诊断结果，不是物理无glitch或可听品质证明。图形设备仍为Intel Arc硬件离屏路径，未Present或显示窗口。

### GPU回收耗时的新定位

同一程序将close之后的工作循环退出、renderer/backend析构和join拆开计时。`joinWaitMs`包含worker收尾，不能再与Drop或loop时间相加成“总工作时间”。

| 模式 / case | shutdown总计 | close→loop退出 | renderer Drop | join等待 |
| --- | ---: | ---: | ---: | ---: |
| poll / 短 | 553.8914ms | 30.2914ms | 521.1506ms | 553.8762ms |
| poll / 长 | 91.2516ms | 0.0282ms | 89.8943ms | 91.2291ms |
| event / 短 | 31.0976ms | 7.1956ms | 22.9670ms | 31.0879ms |
| event / 长 | 26.6418ms | 7.0337ms | 19.0158ms | 26.6325ms |
| event-mmcss / 短 | 104.6868ms | 0.0187ms | 103.4618ms | 104.1936ms |
| event-mmcss / 长 | 30.1939ms | 7.1062ms | 22.0336ms | 30.1832ms |
| event-mmcss重复 / 短 | 356.8916ms | 0.0234ms | 353.1133ms | 356.8695ms |
| event-mmcss重复 / 长 | 35.2712ms | 8.3296ms | 26.1875ms | 35.2612ms |

poll短case关闭时已无in-flight，总553.9ms主要落在renderer Drop区间的521.2ms；MMCSS重复短case亦无in-flight，Drop仍需353.1ms。因此慢关闭不能全部算作工作循环仍在渲染，也不能因另一个event case仅27ms便认为问题消失。

Drop区间包括ClearState/Flush、COM资源析构、驱动返回和线程调度等可能成本；本轮只定位了代码区间，不能进一步把全部时间唯一归因于显卡、驱动或CPU调度，更没有证明不存在资源泄漏。所有正常关闭最终joined且无error/drop panic，并不提供异常驱动永久阻塞时的强制线程回收保证。

### 创建与准备仍需分开看

| 模式 | session创建＋prepare：短 / 长 | prepare单独：短 / 长 |
| --- | ---: | ---: |
| poll | 1448.6384 / 1017.0454ms | 152.5179 / 124.7954ms |
| event | 791.5184 / 499.1821ms | 48.7263 / 53.5980ms |
| event-mmcss | 3241.0899 / 616.1797ms | 116.4555 / 48.5806ms |
| event-mmcss重复 | 1776.5235 / 616.8277ms | 124.2225 / 48.5784ms |

MMCSS仅在实际播放阶段进入，准备/创建未注册，因此该表不是MMCSS初始化加速/变慢的因果实验。短case仍可超过3秒，不能挑prepare的48ms称为完整原生启动延迟。

### 测试与恢复路径核对

独立复核记录见 [verification-summary.json](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/verification-summary.json)，同一可执行文件和源码哈希见 [build-context.json](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/build-context.json)，检查结果见 [verification-checks.json](../src-tauri/target/native-event-validation/batch-539310d9-b986-4799-a06f-2e542ea454a4/verification-checks.json)。复核重新计算所有消费帧的RGB/预期RGBA及PCM区间哈希，并核对PTS、代次、队列计数守恒和MMCSS恢复状态。四次运行记录的48个自有decoder PID及探针进程结束后均无残留。证据位于本地忽略的target目录；工作区有既有未提交改动，Git基准提交不能单独表示本次程序。

本轮前端231项、lint、前端build通过；普通Rust100项通过、6项既有环境用例忽略；stream example72项通过、1项旧真实sidecar用例默认忽略。本轮没有重复执行该旧ignored用例，实际设备证据来自上述四次完整pipeline运行。native example编译及Rust格式检查通过；wrapper模块排序修正后重新构建，再固定用于比较的程序。

只读代码复核确认：Avrt注册后先由guard持有handle再设置NORMAL，后续失败显式恢复；恢复失败保留token和错误供Drop重试，成功才清handle。guard不能跨线程发送；不以CloseHandle代替AvRevert。事件句柄比client更晚析构，Start/Resume只在Stopped时清信号，Wait失败/超时不会变为poll。这些实现检查与成功路径实测互补，不把未实际注入的OS失败称作已经发生。

## 未关闭的问题与下一步

本轮事件路径的最大供给gap约10–13ms且MMCSS重复接近前次，是有用的局部证据；四组仍保持`realtimePerformanceQualified=false`。原有154.3662ms供给间隔、2754.2575msGPU回收没有被抹去，本轮也仍有356–554ms回收和秒级创建抖动。

下一步应优先缩小renderer/backend析构和常驻decoder生命周期的耗时范围，并扩大受控负载/设备矩阵检验event调度；不能把短时功能成功直接作为替换主应用播放器的批准。现阶段仍是4秒完整源＋20秒源部分读取的调试探针，不是长时实时保证或物理音画同步验证。

## 一手来源

- [S1] [IAudioClient::Initialize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize)：shared EVENTCALLBACK的duration/periodicity必须同时0；实际容量读取与事件模式要求。
- [S2] [IAudioClient::SetEventHandle](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-seteventhandle)：Initialize之后、Start之前注册，Start时nonsignaled。
- [S3] [CreateEventW](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw)：auto-reset、初始状态、命名及继承行为。
- [S4] [AvSetMmThreadCharacteristicsW](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avsetmmthreadcharacteristicsw)：关联调用线程、TaskIndex、失败和恢复要求。
- [S5] [AvSetMmThreadPriority](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avsetmmthreadpriority)：NORMAL=0及相对任务优先级。
- [S6] [AvRevertMmThreadCharacteristics](https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avrevertmmthreadcharacteristics)：必须同注册线程执行，检查失败。
- [S7] [Multimedia Class Scheduler Service](https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service)：Audio默认profile、调度影响因素和系统timer注意事项。

上述七页均在本轮实际通过HTTP200读取。单独的AVRT_PRIORITY枚举URL返回404，因此NORMAL数值依据可访问的AvSetMmThreadPriority官方方法页，不引用未读到的网页。官方契约、代码恢复路径核对与四次设备运行的范围分别保留，不互相替代。
