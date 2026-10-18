# Windows 原生GPU跨会话复用与20秒供给探针

独立复核与程序身份：[verification-summary.json](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/verification-summary.json)、[build-context.json](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/build-context.json)。两份文件在重新计算PCM/RGB/预期RGBA哈希、核对完整EOF、代次、累计计数差值与进程退出之后生成。验证器使用Windows原生路径解析保留输出目录限制；初次纯脚本路径兼容错误未改变或重跑媒体证据。

**状态：同一程序的scoped/full、shared/full及一次shared重复均通过本轮功能与供给预算检查；总体实时质量仍未关闭。** 实测shared跨两个case保留同一D3D worker/backend，少执行一次创建与销毁，并完成包含暂停/恢复的20秒源覆盖。正常KaiCut播放器和UI未切换，没有Present、可见窗口、桌面控制或录音，PCM仍进入探针自有静音session。

## 比较方式

沿用独立`native_stream_probe`，增加两个显式选项，默认保持旧行为：

| 参数 | 取值与含义 |
| --- | --- |
| `--renderer-lifetime scoped` | 默认；每个case独立创建、停止并销毁renderer/worker |
| `--renderer-lifetime shared` | 外层持有同一个renderer/worker，各Playback只借用；case结束退役旧epoch，整个流程最后才shutdown一次 |
| `--long-playback partial` | 默认；20秒源只播放0→2秒及10→约11秒再取消，沿用前轮范围 |
| `--long-playback full` | 本轮完整20秒源至EOF，并保留seek/cancel验证；不能与partial数据混称 |
| `--audio-scheduling event-mmcss` | 本轮两组固定使用同一事件/MMCSS策略，不同时改变音频调度条件 |

固定同一二进制、4秒和20秒生成素材、reference、端点及代码，先运行`scoped/full`，再`shared/full`；必要时追加一次明确标注的`shared/full`重复，保留全部结果。事件模式仍按shared EVENTCALLBACK要求duration/periodicity=0/0，实际buffer读回单列，不沿用poll的100ms假设。

## 复用与取消契约

渲染队列继续最多pending2＋in-flight1＋latest completion1。`retire_epoch`仅在停止音频后的控制阶段执行：同锁推进epoch并清空pending/latest，唤醒worker，再等待旧in-flight离开。成功时必须同时确认当前epoch、pending=0、latest为空、in-flight为空、worker未closed且没有error；不能只看“目前无任务”就复用已经失效的renderer。

旧in-flight可能已经提交GPU Draw；退役无法撤销这次离屏计算，只能在完成发布时校验旧epoch并拒绝其进入新会话。新case不能复用旧clip/PCM映射，decoder、音频源游标和session epoch仍独立重建。已经从完成槽取出的结果也要由当前case核对epoch、源index/PTS/hash。

正常case取消明确区分：

- 自有音频Stop/Reset，clock/padding归零；decoder取消、kill/wait及reader join完成。
- `scoped`完成renderer shutdown/join并析构该设备。
- `shared`只完成epoch退役，GPU设备与worker **retained**，不能记录笼统的“全部资源已释放”。

整个shared流程结束后由外层显式shutdown一次，记录设备/worker身份和最终Drop/join结果；任何case错误也要先停音频、回收decoder，再由外层收尾GPU。退役失败不能继续下一case，不能把旧错误清空后假装可复用。

退役等待本轮最多请求3秒；超时报告未就绪并保留worker所有权，外层仍必须执行shutdown。这个控制截止时间不是驱动调用的强制中断保证。Rust不能安全强杀卡在图形驱动内部的线程，join仍可能超过预期；不修改TDR、timer resolution或系统优先级规避该限制。

同一个`instanceId`只表示实现跟踪的实例身份，必须结合worker/backend只创建一次、跨case未Drop及最终一次shutdown证据，不用硬编码相同UUID冒充设备复用。shared统计累计，报告应同时保留case开始/结束计数和增量；每个case仍需实际消费本case的新epoch结果，不能借前case完成数满足门槛。

本轮报告schema3：顶层`rendererLifecycle`记录creations、shutdownCalls、active和created/shutdown事件；每case的`rendererAtSessionStart`与prepare保存实例身份/统计。case取消返回`audioAndDecoderStopped=true`、`rendererRetained=true`、`gpuResourcesReleased=false`及退役结果，此时scoped的GPU也尚由外层持有，随后外层才销毁；shared则保留给下一case。最终顶层shutdown记录才确认整个流程的GPU回收。

## 完整20秒验收

本轮范围仍限制48kHz双声道端点、受控MP4/WAV、160×90 RGB离屏帧。实际使用既有Apache-2.0自有代码和独立LGPL-2.1-or-later FFmpeg7.1/ffprobe；来源、commit和哈希沿用 [binaries/README.md](../src-tauri/binaries/README.md) 与 [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)，不新增codec、下载或重编译sidecar。

`full`要求：

1. Start时应用reader尚未完整读取PCM/RGB，音频8×480块和video4帧高水位保持有界；不能以queueDepth代替生产计数，也不宣称PTS ffprobe没有提前分析并写满OS pipe。
2. 20秒源完整提交960000音频帧，逐样本序列hash与独立参考一致；不是仅验证前缀或跳到终点。
3. 视频完整decoded EOF、实际time_base/PTS检查及最后源帧599已经取得GPU验证完成；允许既有有界renderer丢弃过期任务，但不能缺末帧就报Ended。
4. 三个decoder正常退出、音频待提交块和queue耗尽、deviceclock到20秒、padding=0后才能完成。流程包含约1秒处的暂停/恢复，因此称“完整20秒源覆盖”，不冒称无暂停连续20秒；实际播放与暂停经过时间分开记录。
5. pause/resume、精确顺序重启seek与cancel仍核对源区间、epoch、PCM hash、图形完成和自有进程回收；MMCSS各实际播放阶段显式恢复。
6. 记录供给gap相对实际endpoint预算、等待/超时、原始时钟可靠覆盖、下溢观察及图形结果；`functionalPassed`与`realtimePerformanceQualified`继续分开。

本轮是20秒完整受控源验证，**不是10分钟、复杂多轨或压力/设备矩阵**。前轮partial结果维持原定义，不因参数新增被追认成完整20秒。

每case观察trace上限调整为16384条以容纳长源；phase截止时间按源时长＋8秒设置，20秒源为28秒。上限和deadline是诊断约束，不是预先承诺实时达标；超限明确失败，不截掉异常样本后报通过。

## 计时与结论限制

同一程序分别报告外层renderer创建、每case音频/decoder创建＋prepare、播放、cancel、retire，以及最后renderer Drop/join耗时。shared可能减少每case反复支付的GPU初始化/析构成本，但最后退出仍要析构；每case取消更快不代表最终退出延迟已解决，也不代表总成本凭空消失。

保留历史154.3662ms供给间隔、2754.2575ms图形回收，以及上一批MMCSS重复仍观察到353.1133ms Drop的记录。复用若避开重复开销，只能表述为减少发生次数；不能宣称找到全部驱动/调度原因或证明无泄漏。

设备identity、retire旧in-flight拒绝、slot归零、最后shutdown均应来自真实状态；失败路径先保存结果再做成功判定，不把“发出取消意图”当成已经回收。原始报告和诊断trace保持限长，失败也以新路径保留。

## 运行入口

```powershell
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-reuse-validation' --long-fixture-dir '<同一生成20秒素材目录>' --audio-scheduling event-mmcss --renderer-lifetime scoped --long-playback full
pnpm native:stream-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-reuse-validation' --long-fixture-dir '<同一生成20秒素材目录>' --audio-scheduling event-mmcss --renderer-lifetime shared --long-playback full
```

4秒fixture与reference默认路径、可选`--fixture-dir/--reference-dir`沿用 [NATIVE-STREAM-PROBE.md](./NATIVE-STREAM-PROBE.md)。输出限本仓库target内的新run目录；不覆盖历史证据。默认`scoped/partial`兼容已有命令，本轮对照必须显式写`full`。

## 本机实测

同一程序、同一事件/MMCSS策略和受控素材，按scoped→shared→shared重复执行，三份原始报告均`functionalPassed=true`、供给预算通过、`realtimePerformanceQualified=false`：

- [scoped/full报告](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/run-b31ff356-5261-4a7a-bd10-5e00c396437e/native-stream-validation.json)。
- [shared/full报告](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/run-7fb30f14-f2ed-4c37-b806-daec82a53b8b/native-stream-validation.json)。
- [shared/full重复报告](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/run-298a81a1-9a5f-4e93-8052-6c400210d367/native-stream-validation.json)。

实际硬件为Intel Arc，adapterFlags0、featureLevel11_0，离屏分辨率160×90；音频48kHz双声道，event buffer读回1056帧即22ms。

| 生命周期计时 | scoped/full | shared/full | shared重复 |
| --- | ---: | ---: | ---: |
| GPU创建 / shutdown次数 | 2 / 2 | 1 / 1 | 1 / 1 |
| GPU创建耗时 | 129.2851、60.4034ms | 94.2249ms | 94.6265ms |
| 每case退役：短 / 长 | 2.9168 / 2.2743ms | 2.7594 / 0.0063ms | 3.1272 / 3.6741ms |
| GPU shutdown耗时 | 17.6963、12.9947ms | 最终12.0331ms | 最终19.2827ms |
| 音频/decoder创建＋prepare：短 / 长 | 605.6523 / 313.8115ms | 527.6950 / 303.0518ms | 488.7370 / 299.1795ms |

最后一行不包含GPU创建，GPU单列在顶层created事件；不能与旧版包含GPU的session创建数字直接混算。退役只清旧epoch并等空闲，不析构GPU，因此不能把约0–4ms退役当作GPU销毁速度。确定收益是shared在这两个case之间避免一次创建/销毁，而非证明驱动Drop本身已经变快。

scoped两个case分别使用`81d5016c-fd6a-4106-921f-5e9455a72652`和`843f3c0c-29a2-44cd-8d7f-ac4e7a7237e9`；shared两个case都使用`391db8d2-30fb-4d9f-bee4-e5f947601912`，重复运行则都使用`8faf2c75-7833-4909-af81-36de9f061bed`。每组身份同时出现在created、case开始、prepare、retire和最后shutdown记录；shared长case开始时保留短case累计统计，未出现中间创建/销毁事件。instance ID是owner/backend实例标识，不是物理显卡的全局身份。

所有case取消记录`audioAndDecoderStopped=true`、`rendererRetained=true`、`gpuResourcesReleased=false`；退役后pending/latest/in-flight全空且worker仍open。最终顶层`rendererLifecycle.active=false`、shutdown均joined且无error，才表示外层结束持有。

### 20秒覆盖与供给

| 长源结果 | scoped/full | shared/full | shared重复 |
| --- | ---: | ---: | ---: |
| 完整提交PCM帧 | 960000 | 960000 | 960000 |
| 最后verified视频帧 | 599 | 599 | 599 |
| 设备时钟覆盖源时间 | 20秒 | 20秒 | 20秒 |
| initial＋resume实际经过时间 | 20006.9570ms | 20007.4820ms | 20007.3184ms |
| 独立暂停观察 | 200ms | 200ms | 200ms |
| 最大供给gap | 10.3360ms | 10.5136ms | 10.3945ms |
| event signal数（含seek阶段） | 2104 | 2104 | 2105 |
| event timeout / 观察下溢 | 0 / 0 | 0 / 0 | 0 / 0 |
| Start前应用reader：PCM / RGB | 5760 / 7帧 | 5760 / 7帧 | 5280 / 7帧 |

完整源结束时三个decoder正常EOF、PCM源hash匹配、deviceclock达到20秒且padding0，最后源视频599已经GPU验证完成。实际20秒覆盖包括约1秒处的暂停/恢复，表中的active经过时间不含200ms冻结观察；不称为无暂停20秒，更不是10分钟验收。随后10秒重启seek→约11秒及cancel的切片hash、epoch与资源检查也通过。

三组短源最大供给gap分别11.1664、10.2378、10.2175ms，均低于本次22ms实际缓冲预算。所有播放阶段MMCSS注册与显式恢复成功，event timeout和观察下溢为0；这不等于物理无glitch或全平台硬实时保证。

解码队列高水位维持audio8块/video4帧；Start前reader读取量小于整源，未把PCM/RGB全量缓存进应用。此结论不包括PTS ffprobe内部分析或OS pipe缓存。长case观察记录分别2390、2344、2342条，均在16384上限内。源媒体指纹、完整/seek PCM hash和实际已消费视频内容核对通过；末帧验证不等于600个源帧全部都显示，本轮完全没有Present。

### 验证与保留限制

本轮前端231项、lint、前端build通过；默认Rust100项通过、6项既有环境用例忽略；stream example76项通过、1项旧真实sidecar用例默认忽略，本轮未单独重跑该ignored用例。native example构建和格式检查通过。真实设备证据来自上述三次完整pipeline，单元测试不能与其次数混计。

最终检查记录见 [verification-checks.json](../src-tauri/target/native-reuse-validation/batch-5ab14f97-5fba-4ac9-9bff-a718ce5950ba/verification-checks.json)。独立进程检查未发现仍存活的本次decoder或探针；某个退出后的PID已被系统其它进程复用，按实际进程身份记录而未误判或终止它。

历史154.3662ms供给gap、2754.2575ms图形回收以及353–521ms Drop仍保留。本次最后shutdown约12–19ms，只是当前短单机正常路径结果，不是旧异常已根治的证据。复用避免重复成本，最后退出仍会析构，不能称最终退出延迟已解决或证明没有泄漏。

## 下一步

本轮仍不集成主应用、不显示画面，不把离屏GPU/静音PCM的通过称为可见/可听体验或物理音画同步。下一步评估更长播放、常驻decoder快速seek及最终呈现器；跨会话复用有效不替代实时质量、设备矩阵与最终退出延迟门槛。

依据为仓库已有 [事件音频/MMCSS](./NATIVE-EVENT-AUDIO-PROBE.md)、[流式解码](./NATIVE-STREAM-PROBE.md)、[有界队列](./NATIVE-QUEUE-PROBE.md) 和 [D3D离屏](./NATIVE-D3D-PROBE.md) 的官方API与证据边界；本轮不新增外部研究或扩大系统能力范围。
