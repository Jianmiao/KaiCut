# Windows 原生异步帧队列探针

**状态：120ms人工渲染延迟压力运行与0ms普通运行均已在本机通过本轮窄范围验收。** 上一轮 [D3D11离屏探针](./NATIVE-D3D-PROBE.md) 验证了实际Draw/读回像素正确，但同步readback仍会占用PCM供给线程。本轮把播放期间的图形工作移到独立线程，实测积压受容量限制、PCM供给继续，普通运行中也观察到一次旧epoch的实际GPU完成结果被拒绝。它仍不是屏幕呈现或主编辑器验收。

独立`native_queue_probe`通过非默认`native-queue-probe` feature启用，复用D3D/PCM探针；纯队列由`native-preview-queue` feature显式提供。正常KaiCut播放器、UI、项目schema、FFmpeg二进制与MCP工具不因此改变。本轮仍无可见窗口、swap chain/Present、桌面控制、屏幕捕获或录音；PCM进入自有静音session，不发出测试音。

## 线程与所有权

| 执行方 | 持有内容 | 不应承担的工作 |
| --- | --- | --- |
| 音频/调度主线程 | WASAPI对象、实际PCM提交游标、音频主时钟与源PTS选择 | PCM播放期间不等待帧队列可写，不执行媒体D3D编译/Draw/Map/读回，不在GPU锁内填音频 |
| 单一渲染worker | 在线程内创建、使用、释放D3D renderer及immediate context | 不访问或跨线程移动WASAPI对象，不在共享队列锁内执行GPU/解码调用 |
| 有界共享状态 | pending任务、epoch、最新完成结果、计数和终止状态 | 不保存无限帧副本或无限历史trace，不以结果替换覆盖不可恢复的worker错误 |

D3D11 device可以被多线程使用，但device context必须串行访问。此探针选用更小的约束：整个renderer保留`!Send`并在worker内构造/销毁，主线程仅传有界描述符或共享只读帧引用，不试图通过Mutex把COM图形对象变成跨线程工作池。[S1]

启动准备阶段保留一次主线程非对称校准Draw，发生在PCM Start之前；校准renderer随即释放。实际媒体播放使用的另一renderer只由worker创建/操作/释放，不能把准备校准耗时混入音频供给循环的异步性能结论。

PCM供给仍约每2ms检查可写容量，时钟读回来自真实`IAudioClock`，不是renderer完成时间。2ms是轮询意图，不是Windows硬实时保证；实际供给间隔、可靠采样覆盖和下溢观察须记录。

## 队列容量与丢弃策略

本探针配置**最多2个pending＋1个worker in-flight＋1个latest completion**。这些限制只约束队列中的对象数量，不为任意泛型payload提供字节预算；输入RGB/PCM仍受原短夹具内存限制，任务不得无界复制图像。

- 音频线程提交使用`try_lock`。锁忙则本次帧提交计为busy drop并返回，不能阻塞等待worker；这不改变音频源游标。
- 取得锁且pending已满时，淘汰最旧pending再加入当前帧；保留最新两项，worker取队首。这里的“latest wins”是积压淘汰，不声称worker会抢占已经开始的旧Draw。
- worker取任务后立即解锁，独立执行可选压力延迟、D3D Draw及验证。完成后重新加锁校验epoch/closed，再发布一个最新结果。
- 未被主线程取走的完成结果可被后来的同epoch结果替换，计为overwritten result；不能把替换次数算作源解码丢帧或屏幕丢帧。
- 统计区分attempted、accepted、busy drop、pending evicted、worker taken、published、stale completion、overwritten result、closed拒绝和pending high-water。固定宽度计数器饱和而不是回绕；饱和时不能再把差值当作准确新增数量。

120ms人工renderer延迟是有意制造比30fps源慢的消费方，验证容量/丢弃策略和音频隔离；它不代表真实GPU耗时，更不能用于宣传渲染性能。压力下允许帧任务被丢弃，但不能无限积累排队内容或让PCM供给等待这些帧。

## epoch、发布顺序与取消

seek/cancel推进epoch时，在同一共享锁内清除pending和latest completion，再发布新的epoch。当前探针在暂停/取消时先Stop音频，再立即推进渲染epoch，随后才做250ms/150ms冻结观察；不让整个冻结等待期间继续接受旧epoch结果。worker结果的epoch检查与结果写入在同一锁内完成，不能先检查、解锁、随后写回，否则seek可夹在两步间使旧结果复活。

已在worker执行的旧任务不能被队列清空撤销；它可能仍完成一次离屏GPU Draw。完成时若epoch已变化，结果标记stale并丢弃，不交给新epoch消费者。由于本轮没有Present，这只是过期离屏计算；不能据此声称未来真实显示管线已经具备“取消后绝不显示旧画面”的保证。

消费者取完成结果时也核对当前epoch。若控制命令和消费在同一音频主线程执行，其顺序由该线程保证；未来多个消费者必须避免“解锁取出结果后seek先发生、旧结果再被使用”的竞态。epoch使用checked increment，溢出应拒绝而非回到旧值。

清空/替换payload的析构成本属于锁持有时间的一部分；本探针共享payload应保持小且便宜释放。`try_lock`避免音频生产者阻塞，不代表控制线程的所有锁操作自动具备实时上界。

worker错误是终止性状态，必须传回主线程并停止当前探针；不能只塞入会被latest-wins替换或seek清空的普通完成槽。图形设备失效、像素验证失败、query超时和worker panic分别保留原因，不用持续丢帧掩盖已经失效的renderer。

## close、唤醒和join的实际保证

正常停止时先停止/重置自有音频流，关闭队列并唤醒Condvar，使等待任务的worker能退出；pending/latest按规则释放，in-flight按实际完成/失败路径收尾，然后join拥有的worker。关闭是终态，不接受新任务或新完成结果；虚假唤醒须重新检查谓词。

队列锁不能跨越人工延迟或GPU调用。人工延迟应能被close唤醒，或至少明确其0–250ms上限；shutdown延迟必须实际测量，不能把Condvar被notify当作线程已退出。

**join没有可保证的硬截止时间。** renderer的GetData/Map有界轮询可在调用返回后检查2秒deadline，但不能强制中断卡在显卡驱动内部的API，也不能安全“杀死”某个Rust线程。Windows TDR的默认两秒指GPU调度器等待抢占的阈值，不等于应用join或驱动函数返回的保证；恢复还可能影响整个图形设备。[S2]

本探针不修改TDR注册表、驱动设置或系统调度。Microsoft将这些注册表键定位于驱动开发测试，明确不建议终端用户或一般应用操纵。[S3] 如worker不能正常收尾，应保留错误与进程处置限制，不把detach漏掉的线程称为已回收。

## 观测与验收设计

沿用四秒已生成MP4/WAV和真实解码PTS/PCM，验证Start约2秒、Stop250ms、Resume至真实EOF，随后Reset到源1.5秒并再次播放/取消。音频保持专属session静音，实际PCM非零内容/完整序列hash、源切片hash与源文件完整性继续检查。

两组运行分开记录：

| 模式 | renderer人工延迟 | 检查重点 |
| --- | ---: | --- |
| 普通模式 | 0ms | 实际硬件Draw/像素校验、帧epoch与正常音频生命周期，不预先承诺零丢弃 |
| 压力模式（默认） | 120ms | pending高水位不超过2，积压会淘汰/锁忙会丢弃，音频线程继续供给且本次观察下溢为0，旧epoch完成不发布到新epoch |

实际报告必须区分任务提交时、worker取出时、绘制开始/结束时及主线程消费时；如果某个时间没有测量，则保持未知，不从一个总耗时倒推出全部阶段。render delay与CPU Draw/readback时间分开，结果仍为离屏观察，不能叫显示延迟或纯GPU计时。

共享完成状态只保留最新一项；报告观察trace另设4096条上限，超出即失败而非持续增长。不能为了“诊断完整”把每帧RGBA、全部完成历史或无界字符串放入共享Vec，使队列名义有界而内存持续增长。压力丢弃仅保留有界描述符/计数，内容验证仍对实际执行的帧成立。

start/resume/seek每个有视频的阶段都要求主线程至少实际消费一个当前渲染epoch的已验证结果，并核对index/PTS/输入RGB hash与解码源条目一致。不能只检查全程累计完成过一帧，否则早期成功会掩盖seek后完全没有新结果。最终先保存shutdown结果，再检查joined、error、failures及全部槽位归零，避免尾端worker失败漏报。

期望关闭的门槛：

1. 普通/压力模式都使用真实硬件renderer和实际PCM；WARP或纯CPU模拟不能冒充GPU结果。
2. pending/in-flight/latest容量符合约定；提交不等待队列，计数不回绕，压力时确实发生背压而非跳过压力设置。
3. 帧epoch与音频seek/reset一致；主动制造in-flight后再换epoch的案例，实际观察到旧完成被拒绝，不仅靠空队列测试。
4. 原始音频时钟可靠覆盖、Stop冻结、Reset归零、源EOF、PCM内容完整性仍达标；丢视频任务不能隐藏音频下溢。
5. 正常close使等待worker醒来并被join；失败/error路径显式停止音频并传播原因。回收标记仅表示控制流程完成，不是系统级COM/GPU泄漏证明。

以上门槛与下文真实运行结果相对应；单次结果不扩展为跨设备、任意负载或物理呈现保证。

## 运行方法

在仓库根目录运行默认120ms压力探针：

```powershell
pnpm native:queue-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-queue-validation'
```

普通模式：

```powershell
pnpm native:queue-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-queue-validation' --render-delay-ms 0
```

显式选择生成素材目录与压力值：

```powershell
pnpm native:queue-probe --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-queue-validation' --fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\workflow-validation\98ee1285-41b7-452a-bbce-e38c83b1a2aa' --render-delay-ms 120
```

`--render-delay-ms`范围0–250，默认120；`--fixture-dir`复用原四秒生成素材。输出只允许本仓库`src-tauri/target`内的新运行目录，不覆盖已有证据。命令仅作为显式诊断入口，正常应用不自动启动音频/图形线程或队列。

别名对应：

```powershell
cargo run --locked --manifest-path src-tauri/Cargo.toml --features native-queue-probe --example native_queue_probe -- --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-queue-validation' --render-delay-ms 120
```

## 本机真实运行

两组均由独立example完成，退出码0、报告`ok=true`：

- [120ms压力报告](../src-tauri/target/native-queue-validation/run-bc793827-ea5e-449f-9cdd-e71725585f67/native-queue-validation.json)。
- [0ms普通报告](../src-tauri/target/native-queue-validation/run-f3e36844-6a52-4dde-90b5-5890b5ef20cb/native-queue-validation.json)。

各目录同时保留`verification-summary.json`和`build-context.json`：前者独立核验实际消费帧的源RGB/预期RGBA哈希、阶段epoch/PTS、队列计数守恒与音频门槛，后者记录二进制及源码哈希。工作区含未提交改动，Git基准提交不能单独代表本次程序。配套记录在运行后生成，源码在构建至记录期间未变。证据文件位于本地忽略的target目录；此文档保留关键结果。

共享驱动的[同步D3D回归报告](../src-tauri/target/native-queue-validation/sync-regression/run-b189746f-25f7-437b-97f7-fff77ff0cc79/native-d3d-validation.json)也通过：152次读回正确，去重覆盖120个源帧，PCM完整性与停止/定位/取消通过。它属于功能回归，不是与异步路径同时进行的性能对照实验。

实际媒体renderer为`Intel(R) Arc(TM) Graphics`，vendorId32902、deviceId32085、adapterFlags0、featureLevel45056（11_0），160×90。两组都执行了硬件校准与实际Draw，没有使用fake renderer或WARP。人工延迟仅用于压力组，不是GPU耗时。

| 任务/队列指标 | 120ms压力 | 0ms普通 |
| --- | ---: | ---: |
| Accepted submitted | 152 | 152 |
| Worker taken | 38 | 152 |
| 实际渲染并验证 | 35 | 152 |
| 同epoch成功发布 | 35 | 151 |
| 主线程实际消费 | 35 | 150 |
| pending积压淘汰 | 108 | 0 |
| pending高水位 | 2 | 1 |
| epoch清理pending | 6 | 0 |
| 渲染前因旧epoch取消 | 3 | 0 |
| GPU完成后旧epoch拒绝 | 0 | 1 |
| epoch清理未消费结果 | 0 | 1 |
| Submit busy drop | 0 | 0 |
| worker failures / 结果覆盖 | 0 / 0 | 0 / 0 |

表中的152只计当前播放任务的成功接收，不含三次故意提交旧epoch被拒绝的测试请求。压力组满足`152 = 108淘汰 + 6清理pending + 38 taken`，其中38项taken又分为35项实际渲染和3项渲染前失效；普通组152项实际渲染只有151项发布，其中150项被消费、1项在取消时清理。这些是任务计数，不是152个不同源帧或屏幕显示帧率。

### 真实epoch事件的证据范围

压力组在暂停、seek、取消三次事件中，各观察到一次人工延迟阶段的旧任务在Draw前失效，最终`staleBeforeRender=3`、`staleCompletions=0`；不能称作三次GPU执行后拒绝。

普通组`renderPauseEpoch`记录2→3时仍有`inFlightEpoch=2`，当时rendered60、published60；后续seek前的统计为rendered121、published120、staleCompletions1，证明该暂停epoch变更后有一次真实GPU结果完成但未发布给新epoch。之后取消4→5清理一个已经发布、尚未被消费的完成结果。只这一次实际撞中了完成后失效，不能夸大成每个pause/seek/cancel都观察到了相同竞态。

两组的三次旧epoch试提交均实际返回`Stale`，未出现Busy。Busy路径另由协调测试验证，若其它运行返回Busy，只能说明本次未接收，不能记成命中了epoch校验。

### 音频隔离与实际消费

| 观测指标 | 120ms压力 | 0ms普通 |
| --- | ---: | ---: |
| start/resume/seek消费完成数 | 14 / 14 / 7 | 60 / 60 / 30 |
| 全程最大PCM供给调用间隔 | 6.7130ms | 15.1784ms |
| 最长单次PCM供给调用 | 1.8176ms | 1.8411ms |
| 最大可靠音频时钟样本间隔 | 13.9157ms | 19.7566ms |
| 提交耗时中位数 / P95 / max | 0.0090 / 0.0139 / 0.0377ms | 0.0102 / 0.0158 / 0.0582ms |
| 观察到的音频下溢 | 0 | 0 |
| 观察trace条目 | 485 | 477 |
| close/join经过时间 | 24.3023ms | 26.4231ms |

每次实际消费结果都通过epoch、帧index、PTS、源RGB hash和像素exact验证。压力组35次消费与普通组150次消费不是把旧epoch输出静默算作成功。共享状态始终有界，最终pending=0、completionPending=false、inFlight=false，线程joined且没有error。

两组完整PCM均提交192000音频帧，f32序列hash与解码源一致；1.5秒seek从72000帧开始，提交切片hash核对通过。Resume结束设备位置1536000/frequency384000达到4秒且padding=0，没有通过补假静音尾部完成。Stop250ms与取消Stop150ms位置冻结，Reset后clock/padding为0、旧音频epoch拒绝；源文件hash/大小/修改时间前后一致。

普通组某些最大间隔反而大于压力组，这是两次短单机运行的调度结果，不能据此声称加120ms延迟能提升音频性能。`underrunObservations=0`仅表示轮询期间未观察到下溢，不证明声卡物理输出无glitch。close/join的24–26ms同样是本次正常路径测量，不能覆盖驱动失效/永久阻塞。

`resourcesReleased=true`表示显式音频Stop/Reset、worker关闭并join及Rust作用域退出，没有系统级句柄/COM泄漏证明。实际报告均为offscreen、presentCalled=false、swapChain=false、visibleWindow=false；没有录音、可听测试音或主应用切换。

最终统一检查：`pnpm lint`、`pnpm test`（231项）、`pnpm build`通过；默认`cargo test --locked`为100通过、6项依赖真实编码环境的测试按原配置忽略；启用`native-queue-probe`的lib为107通过、同6项忽略（新增纯队列7项），`native_queue_probe` example为46项通过（含worker协调8项）；`cargo fmt --check`与`git diff --check`通过。忽略项不算通过，本轮未重跑导出编码专项。测试使用的fake backend只用于确定性并发、失败与关停验证，不作为上面硬件结果来源；默认lib与feature lib、多个example有重复测试，不能相加当独立用例。真实探针均已退出，未留驻后台进程。

## 下一步

下一阶段先收敛原生preview会话及有界流式解码边界，摆脱整段短素材预解码，再验证真实呈现器的swap chain/Present和呈现时钟；可见窗口验证继续遵守用户不控制桌面的限制。当前probe只验证线程隔离与离屏队列，不替换主播放器、不验证真实屏幕帧节奏/物理音频，也不能宣称WebView暖启动问题已解决。

## 来源

- [S1] [Microsoft：Introduction to Multithreading in Direct3D 11](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro)：device与device context的线程访问规则。本轮HTTP200读取。
- [S2] [Microsoft：WDDM timeout detection and recovery](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/timeout-detection-and-recovery)：TDR检测、抢占阈值与恢复边界。本轮HTTP200读取。
- [S3] [Microsoft：TDR registry keys for driver development](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/tdr-registry-keys)：默认阈值与注册表修改范围。本轮HTTP200读取。
- [D3D离屏探针文档](./NATIVE-D3D-PROBE.md)：GetData/Map/Flush/RowPitch与计时定义。
- [真实PCM媒体探针文档](./NATIVE-MEDIA-PROBE.md)：源PTS/PCM、WASAPI session隔离、Stop/Reset/EOF边界。
- `src-tauri/src/preview_queue.rs`：纯有界容器、checked epoch、饱和计数与返回的淘汰/拒绝结果；实际worker锁与生命周期由探针owner负责。
