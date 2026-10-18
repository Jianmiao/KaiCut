# Windows 合成交换链 Present 探针

本轮在既有离屏渲染验证之外，独立验证 DXGI 合成交换链的创建、真实 Present 调用、调整尺寸和回收。交换链不连接 DirectComposition/XAML 视觉树，不创建 HWND、可见窗口或桌面操作，也不调用 DirectComposition Commit。本轮不接入主应用、UI、MCP、音频或 GPU 屏幕显示流程。

`Present` 返回成功只说明该 API 调用被接受，不等于图像出现在屏幕、不等于合成器提交或扫描输出，也不证明帧率/显示延迟。未绑定视觉树的交换链是否在本机接受 Present 需要实测；微软接口文档没有承诺这种探针在所有设备上都返回 `S_OK`。失败时记录真实状态，不通过额外创建窗口改变验收范围。

## 官方接口核对

以下内容于 2026-10-17 读取微软官方 API 文档核实；文档描述与本机运行结果分开记录。

| 事项 | 官方约束与本轮选择 |
| --- | --- |
| 创建 | [CreateSwapChainForComposition](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgifactory2-createswapchainforcomposition) 不接收 HWND；要求有效 D3D 设备，软件驱动不受支持。本轮只接受硬件 D3D11 设备，不用软件渲染回退。`GetHwnd`、`GetCoreWindow`、全屏等窗口交换链方法对 composition chain 无效。 |
| 描述符 | 创建方法要求 `FLIP_SEQUENTIAL` 和 `SCALING_STRETCH`。[DXGI_SWAP_CHAIN_DESC1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_swap_chain_desc1) 要求 composition 宽高非零；flip 格式允许 `R8G8B8A8_UNORM`，采样 Count=1、Quality=0，缓冲数量为 2–16。本轮使用 RGBA8、2 个缓冲、不启用 MSAA、Stereo=false、Flags=0。 |
| Alpha | [DXGI_ALPHA_MODE_IGNORE](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ne-dxgi1_2-dxgi_alpha_mode) 表示忽略透明度；本轮不透明视频采用该模式，不从此推断透明合成已经验证。 |
| 视觉树 | [IDCompositionVisual::SetContent](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositionvisual-setcontent) 可把 `IDXGISwapChain1` 作为 visual 内容；[Commit](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-commit) 才使批量 DirectComposition 命令生效。本轮不执行这些视觉树连接步骤。 |
| Present | [Present](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present) 的 flip 同步间隔 0 可丢弃被更新帧替代的帧，不能解释为每次调用都扫描输出。成功 Present 会解绑 backbuffer 0，后续绘制必须重新绑定需要的目标。 |
| 不阻塞标志 | [DXGI_PRESENT_DO_NOT_WAIT](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-present) 在原本要等待时返回 `DXGI_ERROR_WAS_STILL_DRAWING`，不能计为成功提交；`DXGI_PRESENT_TEST` 只检查状态，不属于真实 Present。即使使用 DO_NOT_WAIT，也不宣称任意驱动调用有硬实时上限。 |
| 状态分类 | 保留原始 HRESULT，分别统计 `S_OK`、忙、其他状态和失败。[DXGI_STATUS](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-status) 明确 flip-model 不返回 `DXGI_STATUS_OCCLUDED`，因此不能把未连接视觉树等同于收到 OCCLUDED；若实测出现仍保留原始值。正数成功状态也不自动等同 `S_OK`。 |
| 调整尺寸 | [ResizeBuffers](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-resizebuffers) 前释放全部 backbuffer 直接和间接引用，包括纹理、view、context binding 及相关 command list。`ClearState` 用于解除 context 状态，不替代应用持有 COM 引用的释放。composition 尺寸不可为 0；成功后重新 `GetBuffer` 并建立 view。 |
| 设备丢失 | [DXGI_ERROR](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-error) 的 REMOVED/RESET 需要停止使用失效设备；[GetDeviceRemovedReason](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11device-getdeviceremovedreason) 提供诊断原因。本轮记录原始错误、终止当前实例并回收，不伪装恢复成功。 |

## 验证边界

常规路径把解码 RGB 上传并经 GPU 绘制到交换链 backbuffer，再执行真实 Present，不逐帧等待 CPU 读回。诊断读回单独标记，用于首次与 Resize 后验证实际 backbuffer 像素、行跨度和摘要；不能把普通非读回帧也记作逐像素检查通过。

纯控制状态以代次拒绝旧提交，控制边界、重建和关闭后不能将旧帧提交到新交换链状态。Resize 只在控制阶段执行，先清引用和旧任务，再重新建立尺寸相关资源并复验。报告保留 Present 原始返回值、请求描述符、设备身份、诊断结果和清理结果；当前 `composition_info` 是请求信息，不声称通过 `GetDesc1` 回读全部实际字段。实际 `GetBuffer` 目标的尺寸、格式、采样数与几何约束在绘制前核对。

本轮直接将固定着色器绘制到交换链 backbuffer，正常帧没有 CopyResource、Map 或 GPU 完成查询；CPU RGB→RGBA 转换与上传仍然存在，不能称为零拷贝。对象关闭计时表示本线程 Rust/COM 所有权释放，不证明驱动已立刻回收所有显存。失败设备码保留在错误文本中，模拟错误分类不当作真实设备丢失覆盖。

性能计时只报告测量范围：CPU 调用时间不等于纯 GPU 时间，Present 接受不等于 GPU 执行完成，backbuffer 读回正确不等于屏幕合成正确。已有 AV 离屏验证不自动成为本轮 Present 验证，两者的运行与证据分别保存。

## 当前状态

最终批次 `batch-1d12951d` 的 [第一次 Present 运行](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/run-413c306e-861c-4d65-b288-c9197677e65b/native-present-validation.json) 与 [第二次运行](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/run-9c40769d-44df-401e-9dbb-c1c08378a54e/native-present-validation.json) 均在 Intel Arc 硬件、D3D11 feature level 11_0 上完成，无软件回退。

| 每轮实际结果 | 数量 / 状态 |
| --- | --- |
| 真实 Present 调用 | 47 次，均为原始 HRESULT `0x00000000`（S_OK） |
| 忙、遮挡、其他状态或失败 | 本轮均为 0，不声称真实覆盖了这些分支 |
| 明确请求的 backbuffer 诊断读回 | 6 次，RGB→RGBA 内容逐像素与摘要均一致 |
| 不读回的常规帧 | 41 次；代码路径及返回记录显示没有附加 Map、CopyResource 或完成查询，未采集外部 GPU 调用跟踪 |
| 实际 ResizeBuffers | 4 次：160×90、320×180、1920×1080、161×91 |
| 零尺寸控制 | 1 次挂起，不将零尺寸传给 ResizeBuffers，也没有调用 Present |
| 无效尺寸、旧代次和关闭后请求 | 在控制边界拒绝，不改变已确认尺寸、不新增 Present |

6 次诊断分别覆盖初始 161×91 非对称图、160×90 实际解码帧 0 与 29、恢复后的 320×180、1080p 图案以及缩回 161×91。实际 RowPitch 为 644/640/1280/7680 字节，对应宽度×4，本机这几次没有行 padding；不能因此宣称真实 padding 场景已覆盖。30 个实际源帧来自既有四秒 RGB 参考，只读了其中前 30 帧；其余帧是明确标记的诊断图案，不是完整视频播放。

两轮对象创建分别为 135.24ms、127.76ms，关闭为 16.89ms、20.46ms；41 次常规 Present 的 CPU 调用最大值分别为 1.157ms、3.6887ms。这些是本批调用及所有权释放计时，不是显示延迟、GPU 完成时间或稳定屏幕帧率。未连接视觉树的调用在本机返回 S_OK，仅据此报告 API 接受，`displayQualified=false` 保持。

## 回归、核验与限制

共享 D3D 绘制路径另跑一次 [既有 AV 离屏回归](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/run-8db868f3-c7a1-46bf-b588-ee37a0737a9e/native-av-session-validation.json)，完成 4 段、3 次定位与取消，45 个消费 GPU 结果通过内容检查，最大供音循环间隔 10.4164ms。这是独立回归，并未把本轮交换链接入音频时钟或 AV 协调器。

[独立核验](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/verification-summary.json) 重新计算非对称图案、真实源 RGB 和 RGBA 摘要，核对两轮全部 HRESULT、6 次诊断、41 次无读回记录，以及 Resize、挂起、旧代次/关闭拒绝的调用计数，结果通过。既有 AV 回归以原验证规则单独复核通过。[程序与源码身份](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/build-context.json) 是运行后采集的两个程序及相关源码摘要快照；实际构建成功由本轮命令记录支持，不把该文件说成运行前保存的构建快照。

[工程检查记录](../src-tauri/target/native-present-validation/batch-1d12951d-f498-46c9-a3c8-c1d944e0f173/verification-checks.json)：前端 lint、231 项测试及构建通过；默认 Rust 100 项通过、6 项既有环境专项忽略；AV session 探针 119 项通过、2 项忽略，Present 探针 23 项通过、无忽略。共享模块测试不能相加作为独立覆盖。release 构建和格式检查通过；旧 ignored 项未另跑，真实 GPU 结论来自本轮显式设备运行。

主应用播放器和可见预览保持原路径。本轮没有 HWND、视觉树连接、DirectComposition Commit、屏幕截图或扫描输出证据，不能将 Present 接受与实际屏幕显示等同。

## 复现与下一步

从仓库根目录构建并显式指定素材和新的输出目录：

```powershell
cargo build --locked --release --manifest-path src-tauri/Cargo.toml --features native-present-probe --example native_present_probe
& 'E:\KaiCut（开剪）\src-tauri\target\release\examples\native_present_probe.exe' --fixture 'E:\KaiCut（开剪）\src-tauri\target\native-d3d-validation\run-ec927ae2-fcfe-4e7c-8ce0-475746a10284\decoded-video.rgb' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\native-present-validation\reproduction'
```

上述素材是本机历史验证生成的 120 帧、160×90、RGB24 文件，总长 5,184,000 字节。其他机器须先按媒体探针流程生成并校验自己的同规格素材，再替换绝对路径；不能依赖默认历史 run 目录存在，也不能只用长度相同判断素材身份。输出必须位于本 checkout 的 `src-tauri/target` 内，程序创建新的 UUID 子目录并拒绝覆盖证据。

下一步将此呈现接口接入已有 AV 工作队列，验证背压、代次和尺寸变化的协同；实际可见 surface 连接与屏幕呈现需单独验收，不能用本轮未连接视觉树的 S_OK 替代。
