# Windows 原生预览宿主与合成目标验证

本轮在已有 DXGI 呈现器之外，验证交换链如何连接本进程拥有的 Win32 窗口，并安全经历尺寸变化、卸载、重新绑定与关闭。使用独立诊断程序创建始终隐藏的普通 HWND，建立真实 DirectComposition target、visual、交换链内容和 Commit 调用。主应用、WebView 播放器和 UI 不变；不显示窗口、不控制桌面、不采集屏幕，也不创建音频设备。

这是窗口连接接口的后台验证，**不是可见预览交付**。隐藏窗口绑定成功、Commit 返回成功、Present 接受以及 backbuffer 像素正确，是四种不同证据；它们均不能证明画面已显示、屏幕延迟、物理音画同步或主应用集成完成。上轮音频时钟队列的证据见 [NATIVE-AV-PRESENT.md](NATIVE-AV-PRESENT.md)，不能直接算成本轮宿主的音画同步覆盖。

## 官方接口与边界

下列约束于 2026-10-17 读取微软官方文档核实。

| 事项 | 官方约束与本轮处理 |
| --- | --- |
| 窗口归属 | [CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd) 要求非空、本进程的 top-level 或 child HWND。本轮只使用自己创建、自己释放的 HWND，不接收任意外部窗口。 |
| Target 层级 | 同一 HWND 最多两个 target，分别位于其子窗口之上和之下；同层已有 target 时创建失败。参数 `topmost` 是相对于该 HWND 的子窗口，不是将窗口设置为桌面置顶。必须释放旧 target 后才在同层重新绑定。 |
| 普通隐藏窗口 | [Window Features](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#window-visibility) 说明未指定 `WS_VISIBLE` 时 CreateWindowEx 默认创建隐藏窗口。隐藏普通窗口仍有 HWND、窗口类型和客户区；`HWND_MESSAGE` 创建的 message-only window 仅传递消息，无 z-order 和显示表面，本轮不把它当成 composition target。 |
| 无激活 | [CreateWindowExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-createwindowexw) 的 `WS_EX_NOACTIVATE` 防止系统前台激活，但不是任意代码都无法激活的全局保证。本轮不调用 ShowWindow、SetForegroundWindow 或 SetActiveWindow；[SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos) 调整尺寸时使用 `SWP_NOACTIVATE` 与不改变位置、z-order 的标志，不使用 `SWP_SHOWWINDOW`。 |
| 可见性观测 | [IsWindowVisible](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindowvisible) 检查窗口及祖先的 `WS_VISIBLE` 状态，不是屏幕像素采集。逐阶段检查样式位、IsWindowVisible 和是否成为前台；其他应用的前台窗口可因用户操作改变，不应把全局前台句柄变化直接判成探针抢焦点。 |
| 视觉树 | [SetContent](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositionvisual-setcontent) 接受实现 IDXGISwapChain1 的对象；`SetContent(NULL)` 移除内容。[SetRoot](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiontarget-setroot) 连接根 visual，`SetRoot(NULL)` 清空视觉树，visual 不能同时成为两棵树的根。 |
| 提交 | DirectComposition 方法按事务批处理，须调用 [Commit](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-commit)。返回 S_OK 只记录调用成功；即使使用 [WaitForCommitCompletion](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-waitforcommitcompletion)，也仅表示合成引擎处理了此前 Commit，不是屏幕扫描输出或物理可见性证明。本轮不靠无限等待推进关闭。 |
| 尺寸与坐标 | 逻辑尺寸/DPI 到物理像素的换算须有限、可检查、拒绝溢出；确认宿主客户区实际大小后再确认目标状态。DPI 数学投影不等于跨显示器 WM_DPICHANGED 集成验收。交换链零尺寸暂停，不把零传入 ResizeBuffers。 |
| 释放线程 | [DestroyWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-destroywindow) 明确不能由其他线程销毁窗口。本轮创建、消息处理、绑定、释放和 DestroyWindow 在同一所有者线程，销毁后核对 IsWindow=false。 |

## 验收计划

1. 建立带代次的 surface 状态边界：未挂载、暂停、尺寸待确认、就绪、失败与关闭时拒绝不合时宜的帧。过期尺寸确认与过期帧不能恢复已关闭或已重新绑定的目标。
2. 真实创建隐藏 HWND，保存样式、线程、客户区及可见性观测；绑定 DirectComposition target、visual 和既有交换链，记录绑定结果及 Commit 原始 HRESULT。
3. 在目标绑定后绘制方向不对称图案并抽样读回实际 backbuffer，以独立计算的 RGB/RGBA 摘要和像素结果验证内容。普通提交不附加 GPU→CPU 读回。
4. 实际调整隐藏 HWND 的客户区与交换链尺寸；测试 DPI 数学投影、零尺寸暂停、恢复与无效尺寸拒绝。最小化状态只通过受控状态输入模拟，不调用 ShowWindow，不称为真实 OS 最小化验收。
5. 卸载视觉树，提交解绑事务并释放 target，再重新绑定同一宿主；旧代次帧不得穿越新的挂载状态。
6. 关闭后拒绝新帧和旧完成，依次释放 visual/target 内容、交换链与设备所有权，最终由创建线程销毁宿主。异常分支也须回收；释放计时不等于显存或 COM 无泄漏证明。
7. 每次使用新目录保留真实报告与进程退出信息；独立核验报告字段、数学参考、数量守恒、状态转移及源码/程序身份。工程检查与共享路径设备回归分别记录，模拟状态不冒充设备结果。

## 实现分工

`src/preview_surface.rs` 只管理代次、挂载/布局/卸载票据、确认尺寸和失败状态，不创建窗口或设备。后端完成相应步骤后才确认票据；状态允许提交不等于 GPU 工作已经完成。它作为共享纯状态模块导出，尚未被主应用播放器使用。

独立 `native_surface_probe` 由 `native-surface-probe` feature 启用。`hidden_surface_host.rs` 拥有普通隐藏 HWND、唯一窗口类与线程 DPI context，使用 `WS_POPUP` 和 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP`。创建时没有 `WS_VISIBLE`；没有显示、前台切换或桌面输入代码。WM_MOUSEACTIVATE 返回 MA_NOACTIVATE，WM_CLOSE 不越过图形清理顺序自行销毁宿主。

`composition_target.rs` 借用该 HWND，并通过呈现器同一个 D3D11 设备的 IDXGIDevice 创建 DirectComposition 设备。它连接 target、visual 与交换链，保存 Commit 原始 HRESULT；不另建软件 GPU 设备。卸载依次尝试清 root、清 content 和提交清理事务，某一步失败也继续尝试后续步骤并保留原始错误。绑定失败会尝试回滚，显式卸载失败保留所有权以便清理重试。

借用 HWND 的接口要求宿主在显式卸载及呈现器释放完成前一直有效。同进程检查不解决未来窗口意外销毁、句柄复用或主应用退出竞态；本轮由单线程、局部变量和显式关闭顺序保证这条前提。实际 Tauri 接入还需要把该前提连接到应用窗口所有权。

`surface_driver.rs` 驱动 7 组图案，每组一次诊断读回与三次普通提交；尺寸覆盖 161×91、320×180、480×270、640×360、恢复、1920×1080 和重新挂载。1.0/1.5/2.0 是明确输入的布局比例，真实 HWND DPI 另行记录；不能把数学比例当成本机发生过 DPI 切换。最小化输入与零宽度输入各挂起一次，真实隐藏 HWND 保留最后非零尺寸。

正常关闭与异常回收均先卸载并释放图形对象，再由创建线程 DestroyWindow、注销窗口类并恢复线程 DPI context。Drop 只是异常路径的尽力回收；实际成功结论须来自显式结果、窗口检查和进程退出记录，不由 Drop 被调用推断。

## 当前结果

2026-10-17 的批次 `batch-1d128cf7-1672-4fa7-82a6-355be9c34759` 完成两轮实际隐藏宿主运行和一次既有 AV 呈现队列回归，三个进程退出码均为 0。两轮 surface 运行均使用 Intel Arc 硬件、D3D11 feature level 11_0，没有软件回退；窗口线程实测 DPI 为 192。

| 指标 | [第一轮](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/run-a9d6582b-cdf1-4716-a28f-8a58ba2222ad/native-surface-validation.json) | [复测](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/run-5a83c4b4-4de4-41a8-8ec5-3607f97a806f/native-surface-validation.json) |
| --- | --- | --- |
| 真实 Present / S_OK | 28 / 28 | 28 / 28 |
| 实际 backbuffer 诊断读回 | 7，全部精确一致 | 7，全部精确一致 |
| 普通无读回提交 | 21 | 21 |
| 挂载 / 卸载 | 2 / 2 | 2 / 2 |
| DirectComposition Commit | 4，原始 HRESULT 均为 0 | 4，原始 HRESULT 均为 0 |
| 布局请求 / 实际 ResizeBuffers | 7 / 5 | 7 / 5 |
| 挂起且未调用 Present | 2 | 2 |
| HWND 状态观测 | 43 次均隐藏、非前台、同所有者线程 | 43 次均隐藏、非前台、同所有者线程 |
| 最终状态代次 | 13，Closed | 13，Closed |
| 图形所有权释放时间 | 28.30ms | 25.97ms |

每轮两次挂起分别来自模拟最小化和零宽度布局；前者仍观察到 `IsIconic=false`，未调用真实窗口最小化。五次实际 ResizeBuffers 对应 320×180、480×270、640×360、恢复至 640×360 和 1920×1080，宿主客户区分别经 GetClientRect 确认。没有真实跨显示器 DPI 事件；三种布局比例只是数学投影输入。

7 次读回的独立 RGB/RGBA 摘要均一致，像素差异与 alpha 错误均为零。实际 RowPitch 为 644、1280、1920、2560 或 7680 字节；本批均等于宽度×4，没有真实行 padding 的设备证据。21 个普通提交无读回字段，代码路径不调用 Map/CopyResource/完成查询；未采集外部 GPU 调用跟踪，不声称每个普通帧都经过像素检查。

每轮先解绑并释放旧 target，再成功重绑同一 HWND；重复绑定被拒绝且不替换现有绑定，重复卸载没有伪造 Commit。无效尺寸与过期布局被拒绝，挂起/卸载/关闭状态阻止提交；更广泛的旧票据、失败和 epoch 耗尽分支由纯测试覆盖，不冒充真实驱动故障。

关闭记录确认图形对象先于 HWND 释放，随后 DestroyWindow 成功、IsWindow=false、窗口类注销且线程 DPI context 恢复。两轮都未观察到设备丢失、真实 DXGI 忙或 Commit 失败；相关错误路径的纯测试不算设备失败实测。对象释放耗时与 HWND 不再存在不证明 COM/显存无泄漏。

两轮最高 Present CPU 调用耗时分别为 1.184ms 和 0.996ms。这只是调用耗时，不是帧率、GPU 执行时间或显示延迟。窗口一直隐藏，`displayQualified=false` 保持。

## 独立核验、回归与工程检查

[独立核验结果 v2](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/verification-summary-v2.json) 为 `verified=true`、无问题项。离线核验器独立生成方向图案 RGB 与 RGBA，重新计算全部 14 次诊断摘要，检查原始 HRESULT 分类、事件顺序、代次、Resize/挂起、解绑/重绑、窗口 style/exstyle、所有者线程与销毁结果。它读取报告，不运行设备或控制窗口；API 状态观测不是截图。

初版核验报告保留为失败：Node 非 native 的 realpathSync 解析 Rust 输出的 Windows 长路径前缀时出现 EISDIR，未能读取两份 surface 报告；AV 回归校验已通过。核验器改用 realpathSync.native 后生成 v2，不改应用源码、不重跑设备、不覆盖旧证据。最终报告与源码身份使用 v2。

[既有 AV 呈现队列回归](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/run-a7e3a791-6b35-45c2-baeb-506feb47739e/native-av-present-validation.json) 仍使用未绑定 HWND 的交换链与独有静音 WASAPI 会话，完成 4 段、3 次定位、暂停恢复和取消。49 次 Present 均接受，5 次诊断内容验证通过，消费 45 个结果（40 个普通结果）；另外 3 个结果在控制阶段清除，1 个旧代次结果丢弃。独立 PCM 参考校验通过，最大供音间隔 10.4313ms，未观察到缓冲耗尽。该回归验证共享路径未退化，不能据此声称附着窗口的音画同步已经验证。

[运行前身份记录](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/pre-run-build-context.json) 在 release 构建确认退出成功后、首个设备运行前保存；[运行后身份记录 v2](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/build-context-v2.json) 保存程序、源码和独立核验器摘要。过程退出、身份一致性和进程回收以 [工程检查记录](../src-tauri/target/native-surface-validation/batch-1d128cf7-1672-4fa7-82a6-355be9c34759/verification-checks.json) 为准。

批次结束检查确认 42 个源码/程序身份与运行前记录一致，回归报告中的 12 个解码子进程均已退出，没有 surface 或 AV 呈现验证进程遗留。这是结束时的进程快照，不是驱动或显存泄漏证明；格式与 diff 检查均通过。

前端 lint、231 项测试及构建通过。默认 Rust 100 项通过、6 项既有环境专项忽略；surface feature 的库测试 123 项通过、6 项忽略，其中新增 surface 状态测试 16 项。新 surface 例程 19 项通过、无忽略；既有 AV 呈现例程 147 项通过、2 项忽略，独立 Present 例程 28 项通过。例程包含共享模块测试，不能相加当作独立覆盖；忽略项不能算通过。release 构建与格式检查通过，设备结论来自上述三次显式运行。

## 复现

先完成构建并确认成功退出，再运行程序，不能在链接尚未完成时执行它：

```powershell
cargo build --locked --release --manifest-path src-tauri/Cargo.toml --features native-surface-probe --example native_surface_probe
& 'E:\KaiCut（开剪）\src-tauri\target\release\examples\native_surface_probe.exe' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-surface-validation\reproduction'
```

程序自行生成诊断图案，无需用户媒体文件。输出须位于当前 checkout 的 `src-tauri/target` 下，每轮使用新的 UUID 子目录并拒绝覆盖。其他机器需替换 checkout 绝对路径；设备、驱动与 DPI 不同可能产生不同状态或失败，应保留原始报告，不以显示窗口作为让隐藏测试通过的补救。

## 后续边界

隐藏宿主验证通过后仍需把 surface 接入实际 Tauri 窗口并核对 WebView 与原生子表面的层级、裁切、输入路由、窗口线程生命周期及真实 DPI/最小化事件。实际可见预览、音频运行时窗口变化、长时间播放和屏幕音画同步另行验收；本轮没有这些结果。
