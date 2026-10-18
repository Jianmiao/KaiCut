# Windows 音频时钟驱动的合成交换链队列

本轮把已有音频设备时钟、变帧率选帧与 DXGI composition Present 接口接在同一验证流程中。目标是验证选帧、有限队列、呈现调用、定位、暂停与取消的协同；主应用播放器、项目文件和 UI 不变。程序不创建 HWND，不连接 DirectComposition/XAML 视觉树，不调用桌面控制。

## 范围与状态

这是独立 Windows 诊断程序，不是可见预览的交付。`Present` 的 `S_OK` 只代表调用被接受，不代表 GPU 已完成、图像已显示、屏幕帧率达标或实际音画同步达标。相关 DXGI 文档与接口约束沿用 [Present 探针记录](NATIVE-PRESENT-PROBE.md)，既有离屏 AV 结果见 [AV 会话记录](NATIVE-AV-SESSION.md)。

音频验证仅使用程序独有且静音的 WASAPI 输出会话，不采集系统声音、不更改端点音量或默认设备。呈现器只在自己的工作线程内创建、使用和释放；音频供给线程通过有限队列交付已有 CPU 帧引用，不执行 GPU 上传、读回或 Present。

2026-10-17 已完成两轮正常设备运行、一轮注入延迟的队列压力运行，以及一次既有 AV 离屏回归。四次进程均正常退出；设备与独立核验结果见下文。此结论不依赖先前独立 Present 与 AV 离屏测试的数字相加。

## 实现入口

`native_av_present_probe` 由 `native-av-present-probe` feature 显式启用。`async_presenter.rs` 封装两帧待处理队列、单槽结果、工作线程和呈现器所有权；`av_present_driver.rs` 负责音频时钟选帧、准备、暂停恢复、三次定位以及取消验证。现有 `async_renderer.rs` 与 AV 离屏探针保留原行为。

工作线程每次调用一次呈现器；遇到 DXGI 忙后发布结果，不在工作线程自旋重试。协调器在下一次供音/准备 tick 决定是否重交同帧；旧帧的迟到忙结果不会清除已经更新的投递。准备有五秒期限，事件记录有总量上限。`--render-delay-ms` 可注入 0–120ms 的可取消工作线程延迟，专门制造队列积压；这不模拟真实 HRESULT，也不代表真实驱动发生过忙。

`backendCalls` 是工作线程在代次锁内预留的调用数量，`presentCalls` 统计呈现器成功返回的实际调用记录。若后端返回错误，是否已经进入 Present 可能只能从错误文本得知，不能伪造完整的失败调用计数；报告的 `scope.presentCalled` 在未知情况下保留 `null`。`completed` 是发布到结果槽的工作结果数量，不是 GPU 完成或扫描输出数量。

## 设计约束

| 事项 | 本轮约束 |
| --- | --- |
| 时间基准 | 从 WASAPI 设备时钟换算项目时间，只选择 PTS 不晚于目标时间的源帧；解码源代次与呈现队列代次分别记录。 |
| 容量 | 有限待处理帧与单槽最新结果，不以无限排队隐藏 GPU 拥堵。音频路径投递使用非阻塞锁，队列忙可稍后重试。 |
| Present 拥堵 | 原始 HRESULT 保留。`DXGI_ERROR_WAS_STILL_DRAWING` 不计作接受，不形成“准备完成”；重新尝试必须有界并允许更新帧取代旧帧。队列锁忙与 DXGI 忙分开统计。 |
| 读回 | 只在音频未运行的准备阶段请求实际 backbuffer 读回，核对源 RGB 与目标 RGBA。正常播放不附加逐帧 GPU→CPU 读回；诊断程序可以在工作线程计算已有 CPU 源帧摘要，但该摘要不证明对应 GPU 内容正确。 |
| 准备 | 预期首帧既经诊断验证、又被 Present 接受后，才启动音频。非接受状态不能推动首帧就绪。暂停恢复时同样先准备。 |
| 定位与取消 | 先停止音频，作废旧队列，再等待已进入工作线程的旧调用返回；退役确认后才允许新阶段提交。已执行或已进入驱动的 Present 不能被 epoch 撤销，不能将作废 completion 描述为撤回屏幕内容。 |
| 错误 | 真实设备失败保留原因并终止当前实例；重试策略不能把设备丢失改写为普通忙。异常必须收回音频与工作线程所有权。 |
| 关闭 | 停止接收新工作、清空待处理及结果、join 工作线程，在创建线程释放呈现器。超时不等于强制终止驱动，释放计时不等于显存无泄漏证明。 |

本轮固定视频尺寸。独立 Present 探针的 Resize 已验证，但不能据此声称音频运行时 resize 已验证。CPU RGB→RGBA 转换与上传仍然存在，也不称为零拷贝。

## 验收与证据

纯测试覆盖队列溢出、旧代次失效、关闭后拒绝、真实工作线程所有权、模拟忙状态的重试、失败和取消。模拟返回码只能证明控制逻辑，不能作为真实驱动拥堵或设备丢失的证据。

实际设备运行保留以下数据，并以独立计算核验内容：

- 每段目标时间、音频时钟样本、选中 VFR 帧索引及 PTS，准备完成与音频启动的顺序。
- 源素材身份、实际送入 WASAPI 的 PCM 摘要、诊断 backbuffer 的 RGB/RGBA 摘要及逐像素结果。
- Present 原始 HRESULT、接受与未接受分类、普通播放无读回记录，以及队列计数和最大供音间隔。
- 暂停冻结、定位退役与取消后拒绝；呈现器实例身份及工作线程释放结果。
- 程序与源码身份、进程退出码、完整失败报告以及复测记录，不覆盖失败批次。

运行通过不自动证明长时间稳定性。主应用可见 surface 接入、屏幕显示延迟、实际音画同步、4K/HDR、零拷贝和 macOS 验收均需后续独立测试。

## 本机实际结果

证据位于批次 `batch-07313efd-bb1f-400e-bab2-1d43462b495f`。三次新呈现器运行都使用 Intel Arc 硬件、D3D11 feature level 11_0、320×180 画面，以及 48kHz 双声道静音 WASAPI 输出；没有软件 GPU 回退或可见窗口。

| 指标 | [正常运行](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/run-4683ccb8-9ef8-46f4-af76-2f60e3da55a1/native-av-present-validation.json) | [正常复测](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/run-34d31f28-5c58-4df7-a93a-59589778c9d0/native-av-present-validation.json) | [120ms 延迟压力运行](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/run-88211143-64ff-4453-82f7-a794b00aed2a/native-av-present-validation.json) |
| --- | --- | --- | --- |
| 已接纳的队列提交 | 49 | 49 | 49 |
| 实际 Present / API 接受 | 49 / 49 | 49 / 49 | 16 / 16 |
| backbuffer 诊断读回 | 5 | 5 | 5 |
| 协调器消费的普通无读回结果 | 40 | 40 | 11 |
| 协调器消费的总结果 | 45 | 45 | 16 |
| 队列最大待处理深度 | 1 | 1 | 2 |
| 队列满时淘汰旧任务 | 0 | 0 | 18 |
| 退役清除的待处理任务 | 0 | 0 | 10 |
| 进入后端前被代次变化取消 | 0 | 0 | 5 |
| 最大供音循环间隔 | 10.3344ms | 10.4191ms | 10.4575ms |
| 观察到音频缓冲耗尽 | 0 | 0 | 0 |

两轮正常运行的 49 次调用中，5 次有 GPU 像素读回，其余 44 次没有读回。协调器各消费了 45 个结果，另有 3 个结果在控制阶段从结果槽清除，1 个旧代次结果在返回时丢弃；这些已发生的 Present 仍保留在后台计数中。已消费结果逐条保留原始 HRESULT，均为 `S_OK`；未消费的 4 个结果只有后台接受计数，不伪称拥有逐条结果记录或像素证据。

压力运行的任务数量守恒为 `49 = 16 次实际调用 + 18 次队列淘汰 + 10 次待处理清除 + 5 次调用前取消`。这验证有限队列与取消策略在注入延迟时生效，不代表屏幕丢帧数。三轮都没有真实 `DXGI_ERROR_WAS_STILL_DRAWING`、遮挡状态或设备丢失；忙分支的证据仅来自纯测试，不能用注入延迟替代真实驱动忙。

每轮均完成 4 段、3 次定位、一次暂停恢复和最终取消。四个起始项目时间为 2.2s、7.123s、0s、9.5s，对应准备帧索引 50、160、0、214；另一次诊断来自暂停恢复。5 次诊断只在音频未运行时完成，实际 backbuffer 摘要与独立解码参考一致，像素差异及 alpha 错误均为零，RowPitch 为 1280 字节。普通帧只核对已有 CPU 源帧身份及实际返回状态，不宣称全部 49 帧经过 GPU 像素核验。

三轮最大供音间隔均低于本机 22ms 音频缓冲时长，压力运行也未观察到缓冲耗尽。暂停 200ms 时音频位置和 padding 保持不变；取消后位置与 padding 归零，退役后与再等 150ms 的 Present 计数分别保持 49、49、16。每轮复用同一个呈现器实例，最终线程 join、队列清空且无错误；关闭耗时分别为 16.69ms、19.37ms、18.17ms，仅表示本轮对象与线程所有权回收时间。

这是短时、低分辨率且设备静音的验证。PCM 对比证明送入 WASAPI 的样本正确，不证明所有已排队样本都实际播放；未录制物理输出，也不能据供音间隔宣称声画同步或长时间播放稳定性达标。`displayQualified=false` 与 `realtimePerformanceQualified=false` 保持。

## 独立核验与工程检查

[独立核验结果](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/verification-summary.json) 为 `verified=true`、无问题项。核验器重新计算已接受 PCM 的连续样本参考，核对源文件身份、准备帧及暂停恢复帧的 RGB/RGBA 摘要、时钟选帧关系、正常无读回结果、队列计数、退役与取消记录。其范围没有扩展到未消费结果的逐帧像素或屏幕显示。

[既有 AV 离屏回归](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/run-ba5ac873-240e-4317-9d9a-5e6648bb94c9/native-av-session-validation.json) 另行完成相同 4 段控制流程，45 个已消费 GPU 结果内容检查通过，最大供音间隔 10.5538ms，未观察到缓冲耗尽。该旧路径仍逐帧读回，不把其结果算作新呈现器的覆盖。

构建完成、任何设备运行开始前保存的有效身份记录是 [pre-run-build-context-v2.json](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/pre-run-build-context-v2.json)；[build-context.json](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/build-context.json) 是运行后的身份快照。首次 `pre-run-build-context.json` 在编译尚未完成时采集，当时新程序尚不存在、设备运行未开始，已由 [invalid 记录](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/pre-run-build-context.invalid.json) 明确作废，不能作为构建成功证据。

[工程检查记录](../src-tauri/target/native-av-present-validation/batch-07313efd-bb1f-400e-bab2-1d43462b495f/verification-checks.json)：前端 lint、231 项测试及构建通过；默认 Rust 100 项通过、6 项既有环境专项忽略。新呈现例程 142 项通过、2 项忽略，其中新 worker 有 19 项不使用设备的测试；旧 AV 例程 119 项通过、2 项忽略，独立 Present 例程 23 项通过。共享模块测试不能相加为独立覆盖；release 构建、格式及 diff 检查通过。忽略项不算通过，真实设备结论来自上述显式运行。

检查记录同时核对 37 个源码/程序身份未变；四次运行记载的 48 个解码子进程均已结束，没有验证程序或所属解码进程遗留。这是批次结束时的进程检查，不是 COM/显存泄漏证明。

## 复现与下一步

从仓库根目录先完成构建，确认构建退出成功后再运行，不能一边链接可执行文件一边启动它：

```powershell
cargo build --locked --release --manifest-path src-tauri/Cargo.toml --features native-av-present-probe --example native_av_present_probe
& 'E:\KaiCut（开剪）\src-tauri\target\release\examples\native_av_present_probe.exe' --fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\native-audio-timing-validation\fixtures-d924c584-e159-44f3-b1d0-678e34c76cd2' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-av-present-validation\reproduction'
```

另起一次运行添加 `--render-delay-ms 120` 可复现队列压力条件，不保证完全相同的调度计数。输出必须在当前 checkout 的 `src-tauri/target` 下，每次创建新的 UUID 目录并拒绝覆盖。示例素材来自本机已有验证夹具；其他机器须先生成并核验自己的 AAC 偏移源、VFR 源、连续 PCM 与视频参考，替换路径，不能依赖该历史目录。

下一步是可见 surface 的连接接口与生命周期：明确宿主所有权、挂载/卸载、尺寸变化、最小化和设备失败行为，继续通过接口与后台测试推进。实际将交换链挂入窗口后的屏幕显示、延迟及音画同步单独验收；本轮不控制桌面，也不以未连接交换链的 `S_OK` 代替可见预览验收。
