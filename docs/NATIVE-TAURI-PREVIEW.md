# Tauri 原生预览子表面集成

本阶段把已有 D3D11／DXGI／DirectComposition 呈现器连接到 KaiCut 实际 Tauri 主窗口的自有原生子窗口，并用 WebView 中实际预览区域的布局驱动子窗口位置与尺寸。这是默认关闭的开发验证入口，不替换正式 WebView 媒体播放器，也不代表完整原生音视频播放已交付。

只有编译 `native-preview-host` feature 并显式传入 `--native-preview-host` 才启用。当前内容是方向不对称的校准图案，尚未连接解码队列或音频时钟。本轮验收使用隔离、隐藏、不抢焦点的 Tauri 实例；没有桌面输入控制、屏幕采集或可见窗口验收。

## 所有权与布局边界

- 原生子窗口、呈现器及 DirectComposition 对象由 Tauri 主线程拥有，不跨线程移动 COM／HWND 所有权。异步 IPC 只携带可检查的布局与会话数据，再投递主线程执行。
- 前端读取实际预览容器的 CSS 边界；Rust 核实有限数值、缩放、父客户区和最大分辨率，依据会话与递增序号拒绝旧布局。客户端提供的数值不直接成为任意外部 HWND 或不受限尺寸。
- 原生表面只覆盖预览画面区域，不覆盖控制栏。与弹窗、菜单、可交互覆盖层冲突时应挂起或隐藏子表面；不能假定 HTML 的 z-index 能盖过单独 HWND。
- 父窗口尺寸或缩放变化时先隐藏并使旧布局失效，再等待新的前端测量。未确认的新尺寸不应继续展示旧位置的图案。
- 创建和调整子窗口均不请求前台或焦点。子 HWND 的可见样式、祖先实际可见状态、DOM 的 visibility 是不同观测，必须分别记录。
- 子窗口自身销毁消息负责清理图形对象，覆盖父窗口直接销毁而跳过 Tauri CloseRequested 的正常路径。若 COM 调用中发生同步销毁重入、呈现器仍被借用，则操作返回后立即回收并报告失败，保留 `cleanupBeforeNcDestroy=false`；不能宣称此极端分支也保证先释放图形再销毁 HWND。最终报告需证明实际清理顺序，不能仅凭 Drop 代码宣称成功。

## 已核实的接口约束

以下微软文档于 2026-10-17／18 读取；Tauri 时序同时对照本地锁定的 `tauri 2.12.1`、`tauri-runtime-wry 2.12.1` 和 `tao 0.37.1` 源码。

| 接口或事件 | 约束与实现含义 |
| --- | --- |
| [CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd) | 可接本进程 top-level 或 child HWND。同一 HWND 每种 target 层级最多一个；target 的 topmost 是相对该 HWND 的子窗口，不是桌面置顶。 |
| [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos) | 子窗口坐标使用父客户区物理像素；NOACTIVATE 避免激活。NOZORDER 会忽略 insert-after，不能同时用它宣称改变了兄弟窗口层级。 |
| [WM_NCHITTEST](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-nchittest) | HTTRANSPARENT 的查找范围是同一线程的底层窗口，不能据返回值就宣称所有 WebView 点击都穿透。布局避让、窗口 style、命中测试与实际用户输入是不同证据。 |
| [WM_DESTROY](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-destroy) | 消息先发给正在销毁的窗口，再发给其子窗口；处理时可假设子窗口仍存在。自己的子窗口过程可在该消息中同步卸载并释放图形所有权。 |
| [WM_NCDESTROY](https://learn.microsoft.com/en-us/windows/win32/winmsg/wm-ncdestroy) | 在 WM_DESTROY 之后、子窗口销毁之后发送，适合清除该窗口关联的 userdata；原生回调不可保留已失效对象指针或二次释放。 |
| [SetWindowRgn](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowrgn) | region 坐标相对窗口左上角；成功后系统接管 region，调用方不能继续删除它。失败仍须回收调用方资源。矩形相交检查不等于已验收圆角裁切像素。 |
| Tauri `window.destroy()` | `tauri-runtime-wry` 直接执行 WindowMessage::Destroy，而不触发 CloseRequested。现有脏项目确认路径可调用它，因此只监听 CloseRequested 不足以保障子表面寿命。 |
| Tauri `run_on_main_thread` | 本地锁定版本在主线程调用时直接执行，其他线程调用时投递事件；操作完成、错误与关闭竞态仍须用完成响应或生命周期状态核实。异步请求已发送不等于布局已落地。 |

## 验收目标与证据范围

下列条目定义接口验收范围；实际完成的设备检查以“实际运行结果”为准，不能把纯测试、代码检查或后续目标全部算作真实窗口验收。

1. 同一构建验证默认入口关闭与显式入口开启，记录真实进程身份、主 HWND、子 HWND、父子关系、所有者线程及非前台状态。
2. 使用实际 WebView 布局完成初次挂载和有限次数布局变化，独立核对 CSS 到物理像素投影、客户区裁切及实际子窗口矩形。
3. 旧会话、旧序号、越界和非法缩放不得改变已确认布局；关闭后不能重新创建子表面。
4. 校准图案使用实际当前 backbuffer 读回；独立生成数学参考并核对 RGB／RGBA 摘要、像素差和 alpha。Present／Commit 接受与内容正确分别记录。
5. 菜单／模态遮挡与控制区避让保留实际结构数据。后台程序只检查自己的布局，不触发真实鼠标键盘输入。
6. 显式关闭、父窗口销毁及异常回收需记录图形释放、WM_DESTROY、WM_NCDESTROY 和子 HWND 失效结果；程序结束后核查其进程退出。
7. 运行报告、退出码、构建身份和独立核验保存在新建批次，失败原件保留，不覆盖为成功结果。

## 实际运行结果

2026-10-18（北京时间）的最终批次为 `batch-952971e6-c385-4d58-9feb-a3cc00017fd3`。两次运行都启动真实 KaiCut Tauri／WebView2，使用独立验证配置和隐藏主窗口，退出码均为 0；没有启动可见窗口或操作桌面。D3D11 使用 Intel Arc 硬件、feature level 11_0，子窗口实际 DPI 为 192。

| 指标 | [第一轮](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/run-c935efff-5a55-43eb-9a2f-69954b6173be/native-tauri-preview-validation.json) | [复测](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/run-fbb2113c-2e37-4c4e-b4a2-cc5dc56f398a/native-tauri-preview-validation.json) |
| --- | --- | --- |
| 布局／生命周期检查步骤 | 6 | 6 |
| 独立子表面所有权 | 2 个不同 surfaceId | 2 个不同 surfaceId |
| 校准 Present／S_OK | 5／5 | 5／5 |
| 实际 backbuffer 读回 | 5，全部精确匹配 | 5，全部精确匹配 |
| ResizeBuffers | 5 | 5 |
| 遮挡期间新增 Present | 0 | 0 |
| 原生主窗与子窗可见 | 始终 false | 始终 false |
| 前端完成状态 | 5 次 attached、1 次 blocked | 5 次 attached、1 次 blocked |
| 清理完成早于 WM_NCDESTROY | 两个子表面均 true | 两个子表面均 true |

6 个步骤依次为初始布局、把实际 `.preview-canvas` CSS 宽度改为 300px、插入受控 dialog 遮挡节点、移除遮挡、请求调整 Tauri 窗口尺寸，以及切换已有预览模式触发卸载／重挂载。它们是隔离实例中的程序化布局检查，不是真实拖拽、菜单操作或键鼠体验验收。

初始 DOM viewport 为 **1280×792 CSS 像素**，原生客户区为 **2560×1584 物理像素**。本轮请求窗口逻辑尺寸为 1180×760；实际得到 **1180×788 CSS 像素／2360×1576 物理像素**。报告同时保留请求值和实际值，不把受原生窗口约束影响的结果改写为请求高度。每步独立核实 CSS 坐标按实际 2.0 比例投影后与子窗口 `GetWindowRect`／`MapWindowPoints` 结果一致，且未覆盖播放控制区。

五次实际校准尺寸依次为 1252×612、600×612、遮挡恢复后的 600×612、父窗口调整后的 600×604，以及重新挂载后的 1052×604。全部 RGB／RGBA 摘要与独立生成的方向图案一致，像素差和 alpha 错误为零。每个校准帧都执行诊断读回；本轮没有测媒体连续播放，也不能把这些带读回的调用耗时当作正常播放器性能。

子窗口具有 WS_CHILD／CLIPSIBLINGS／CLIPCHILDREN 和 NOACTIVATE／NOREDIRECTIONBITMAP，没有 WS_VISIBLE、TABSTOP、桌面 TOPMOST 或 APPWINDOW。检查点中它位于父窗口兄弟子窗口顶部，下一兄弟窗口的所有者线程与当前线程一致。自有窗口过程返回 HTTRANSPARENT、MA_NOACTIVATE；这仍只是结构与协议响应证据，没有派发用户输入来证明 WebView 的实际点击行为。

旧序号和旧会话请求都被拒绝，拒绝前后的完整原生状态一致。重挂载用不同 `surfaceId` 区分所有权，而不假定 Windows 永远不会复用 HWND。父窗口销毁时，Tauri Destroyed 监听先显式清理子表面：两轮 `autoCleanupTriggered=false`、`cleanupBeforeNcDestroy=true`。因此已验证的是 Tauri 事件驱动的正常清理路径，不能宣称子窗口 WM_DESTROY 自动兜底或 COM 重入销毁已经得到设备实测。两次表面释放均保留实际解绑 Commit 成功、子 HWND 失效、窗口类注销和无延后清理结果；这不证明驱动／显存无泄漏。

## 独立核验与保留的失败证据

[最终独立核验报告](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/verification-summary-2026-10-17T16-27-02.478Z.json) 为 `verified=true`，两份主验收运行报告均通过、无问题项。核验器离线重新生成主验收 10 次校准采样的数学参考，验证 SHA-256、原始 Present HRESULT、父子窗口关系、线程、style、实际尺寸、控制区避让、阻塞／恢复、序号拒绝和关闭证据，同时核对两个进程退出记录和 15 个相同构建身份记录。重复出现在 history／snapshot 中的同一帧按 surfaceId 与计数去重，不算多次设备采样。辅助检查具有单独的证据范围，不合并到两轮正常退出结论。

第一批 `batch-65c6f796-e23d-476b-b379-a6b224164f39` 的设备操作已完成，但诊断采样时 Rust 布局已经更新、前端 IPC 完成回调尚未把状态从 pending 改为 attached，因此[初次独立核验失败](../src-tauri/target/native-tauri-preview-validation/batch-65c6f796-e23d-476b-b379-a6b224164f39/verification-summary-2026-10-17T16-17-27.401Z.json)。原始报告保留；修复集成验证程序的采样时序，等待 DOM 完成状态与同一原生 revision 对齐后，在新批次重新构建并运行两次。没有修改旧报告或放宽 pending 的验收条件。

每轮最终运行前均保存程序、前端构建和相关源码摘要：[首轮身份](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/host-check-pre-run-context.json)、[复测身份](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/host-repeat-pre-run-context.json)。进程 34848、32920 分别与各自报告及[首轮退出记录](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/host-check-process-exit.json)、[复测退出记录](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/host-repeat-process-exit.json) 对应，均确认退出码 0。

随后增加 MCP 只读检查，保留了三次不同结果。第一次进程 25900 在集成检查自行退出后，MCP 的后续查询连接失败；它仍产生了[第三份校准报告](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/run-f3fd06f2-278d-43ee-9853-76920c1e6163/native-tauri-preview-validation.json)，几何、5 次像素采样及清理字段独立核验通过，但未采集其退出码，不列入主验收完成次数。第二次进程 26628 过早查询，WebView UI 监听尚未就绪导致超时。两份失败原件分别为 [mcp-inspection.json](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/mcp-inspection.json) 和 [mcp-inspection-live.json](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/mcp-inspection-live.json)。

最终[就绪后 MCP 检查](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/mcp-inspection-ready.json) 通过：`get_state` 与 `ui_inspect` 对应同一空项目／revision，原生宿主启用、实际子窗口存在、主窗隐藏且无焦点。DOM 此时可以报告 `visibility=visible`，不能替代原生窗口的 `visible=false`。独立核验另行检查了该次校准帧摘要；此进程最后由测试工具 SIGTERM 结束，退出码未知，因此不作为正常窗口释放或优雅退出证据。中间一次核验将第三份辅助报告误计为必须具备主验收退出记录，产生的[失败核验原件](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/verification-summary-2026-10-17T16-23-56.114Z.json) 同样保留；最终核验明确记录两个主进程和辅助进程的不同证据边界。

前端 lint、246 项测试、20 项 MCP 测试和构建通过；默认 Rust 102 项通过、6 项既有环境专项忽略，启用宿主 feature 的 Rust 148 项通过、6 项忽略。最终采样时序修订后，25 项宿主专项测试、实际应用构建、格式与 diff 检查再次通过。feature 测试包含共享模块，不与默认测试简单相加；忽略项不算通过。隐藏运行使用 debug 构建，因为验证入口要求隔离的 debug profile；这些结果不是 release 性能测量。

[工程检查记录](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/verification-checks.json) 保存命令结果、15 个源码／构建／程序身份与运行前一致的检查，以及本轮指定的 5 个验证进程均已结束的快照。[验证清单](../src-tauri/target/native-tauri-preview-validation/batch-952971e6-c385-4d58-9feb-a3cc00017fd3/validation-manifest.json) 明确主进程、辅助进程及缺失的退出码；进程已不存在不能补成未知的正常退出码。

默认入口关闭目前由 feature／运行开关的代码分支和默认测试覆盖，本轮没有另外启动一个 feature-off 的真实应用实例验收。无效缩放、非有限值和过期 viewport 主要由纯测试覆盖；设备运行实际核查的是旧序号／旧会话拒绝，以及上表的正常布局、遮挡和生命周期变化。

## 复现入口

完成前端与宿主 feature 的构建、确认构建进程成功退出后，再启动隔离验证进程；不要让构建器与运行中的同一路径程序争用文件。

```powershell
pnpm build
cargo build --locked --manifest-path src-tauri/Cargo.toml --features native-preview-host,tauri/custom-protocol --bin kaicut
& 'E:\KaiCut（开剪）\src-tauri\target\debug\kaicut.exe' --native-preview-host --mcp --mcp-hidden --mcp-validation --native-preview-check-dir 'E:\KaiCut（开剪）\src-tauri\target\native-tauri-preview-validation\reproduction'
```

验证进程只接受当前 checkout `src-tauri/target` 下的输出路径，每轮新建 UUID 目录、拒绝覆盖已有报告。正常启动不带该 feature／运行开关时仍使用原 WebView 播放器。其他机器应替换 checkout 路径并保留实际失败结果，不以显示窗口来修复隐藏验证。

## 不覆盖的结论

隐藏的真实 WebView 与子 HWND 验证可以建立布局、绑定、API 调用和生命周期证据，不能证明用户可见的画面、抗锯齿／圆角效果、屏幕刷新率、真实键鼠交互、HDR／色彩管理、跨显示器 DPI 转换或物理音画同步。本阶段也没有将校准图案变成媒体播放；正式播放器仍走现有 WebView 路径。
