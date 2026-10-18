# Tauri 原生子表面的媒体接入

本阶段把真实 FFmpeg 解码帧、独有静音 WASAPI 会话的音频时钟和已有 Tauri 原生预览子表面连接起来。正式 WebView 播放器仍是默认入口；验证开关只用于隔离的隐藏实例，不是默认播放器切换，也不是可见画面或物理音画同步的验收。

本轮不控制桌面、不采集屏幕或系统声音，不更改默认音频端点或端点音量。媒体源使用此前已独立核验的 AAC 偏移音频与 VFR 视频夹具；源文件、连续 PCM 参考及 VFR 帧索引的身份应在每次运行前后核实。

## 责任边界

- Tauri 主线程拥有子 HWND、D3D11／DXGI／DirectComposition 对象及呈现调用。后台媒体任务只拥有解码器、音频会话和有限帧队列，不能跨线程移动窗口或 COM 所有权。
- 音频设备时钟驱动项目时间，VFR 视频选帧依据真实 PTS；不能把固定 FPS 或壁钟睡眠当作主时钟。
- 准备阶段允许诊断读回实际交换链 backbuffer。普通播放只上传和呈现，不逐帧把 GPU 图像读回 CPU；CPU 源帧摘要也不等于实际 GPU 像素证据。
- 目标尺寸来自实际 WebView 预览区域的物理像素布局，源视频保持自身尺寸。等比缩放后的未覆盖区域填充不透明黑色。本轮使用确定性的整数最近邻采样验证几何，不作为最终缩放质量或色彩管理方案。
- overlay、定位、取消和关闭必须明确作废旧代次。已经进入驱动的 Present 不能被撤回，退役完成后拒绝旧任务才是可检验的边界。
- 仅收到异步请求不等于操作已经完成。准备帧接受、音频启动、窗口布局确认、暂停生效和释放完成分别记录。

本轮 resize 在音频暂停、旧主线程回调退役完成后执行，不据此声明播放中 resize 已验证。overlay 检查保持音频继续，宿主隐藏子表面并拒绝画面提交；去掉遮挡后恢复提交。暂停和取消后的冻结检查会让媒体线程再次实际查询设备时钟，不能只比较两份缓存状态。

准备流程要求已接受当前应显示的帧，并已观察到其后的未来帧，从而排除解码队列暂时为空导致的过早准备。当前开发入口不支持在视频最后一帧／EOF 处准备；后续需以明确 EOF 证据补足该分支，不能取消准备条件来掩盖这一限制。

窗口层级、原生子窗口不能被 HTML z-index 任意覆盖、裁切与销毁顺序的接口约束沿用 [Tauri 子表面接入](NATIVE-TAURI-PREVIEW.md)。音频时钟、PCM 连续性和有界队列的证据基线见 [音画呈现队列](NATIVE-AV-PRESENT.md)。本轮必须有自己的实际媒体运行记录，不能把前两阶段的数字相加作为集成完成证据。

## 独立核验要求

每次运行保留原始报告和错误，另由只读核验程序计算结果：

1. 对准备帧保留实际源 RGB 文件，先与独立 VFR ledger 的对应帧摘要核对，再以 JavaScript 独立计算整数等比缩放、居中和黑边的 RGBA 参考。实际 backbuffer 读回文件须与这一参考逐字节一致，不能只比较报告自述的 `exact=true`。
2. 普通帧核对源索引、PTS、音频时钟到项目时间的换算、代次、实际 Present 原始 HRESULT，以及未请求／未执行诊断读回。普通帧不计为逐像素验收。
3. 对实际交付 WASAPI 的 PCM 捕获，以既有连续 WAV 参考和一次解析的定位锚点独立重建，包括前置静音、源样本边界和尾部静音。排入设备的样本可能在定位或取消时被清空，不等于全部已经物理播放。
4. 分别核对暂停时钟冻结、恢复前准备、定位后新代次、overlay 期间无新增 Present、尺寸变化后的源画面比例和黑边、旧代次拒绝，以及取消后队列清空与计数停止。
5. 保留主进程和所有解码子进程的退出记录。正常退出、对象所有权释放和没有残留进程不是驱动／显存无泄漏证明。
6. 保留运行前后的程序和源码摘要、构建结果、退出码及失败原件。隐藏实例中的 API 接受不能替代真实屏幕表现或用户交互验收。

## 实际结果

2026-10-18（北京时间）在批次 `batch-d4edb7ca-72fc-4069-9330-1fc50a458afb` 完成两轮真实隐藏 Tauri／WebView2 运行。程序使用 Intel Arc 硬件、D3D11 feature level 11_0、DPI 192，以及 48kHz 双声道 float32 的独有静音 WASAPI 会话。主窗口、原生子窗口均保持不可见；没有派发桌面键鼠输入。

| 项目 | [首轮](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/run-16867661-21f1-4b5f-a947-236316208250/native-tauri-media-validation.json) | [复测](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/run-b6f7f099-5e7c-4029-a843-e124746ec211/native-tauri-media-validation.json) |
| --- | --- | --- |
| 主进程 PID／退出码 | 42604／0 | 42272／0 |
| 实际媒体 Present／S_OK | 42／42 | 42／42 |
| 准备阶段 backbuffer 读回 | 5，逐字节一致 | 5，逐字节一致 |
| 普通媒体调用，无诊断读回 | 37 | 37 |
| overlay 宿主拒绝，未调用 Present | 9 | 9 |
| 主线程媒体回调／actor 消费结果 | 51／50 | 51／50 |
| 最大待处理队列深度 | 1 | 1 |
| 最大供音间隔 | 10.4296ms | 10.2975ms |
| 音频缓冲时长／观察到耗尽次数 | 22ms／0 | 22ms／0 |
| 解码子进程退出记录 | 12 | 12 |

每轮 42 次是媒体提交，不包含宿主挂载时的一次校准图案。5 次诊断对应源索引 **50、57、160、0、214**，分别来自初始准备、暂停后恢复准备及三次定位。独立核验先对照 VFR ledger 校验每份源 RGB，再从实际源字节计算等比缩放和黑边，最后逐字节比较实际交换链 backbuffer 文件。普通 37 次只核对源帧身份和实际 HRESULT，不声称每帧都经过 GPU 像素检查。

初始源画面为 **320×180**，目标表面为 **1252×612**，有效画面为 **1088×612**，左右各 82 像素黑边。暂停期间把实际预览容器宽度改为 300 CSS 像素后，目标表面为 **600×612**，有效画面为 **600×337**，上方 137、下方 138 像素黑边。奇数剩余像素留在右侧／下侧是本轮整数规则；这验证几何与不透明黑边，不代表最终缩放画质。宿主与 DOM 的实际布局、父子关系及控制条避让另行核对，尺寸修改是程序化操作，未验收真实拖拽体验。

四段项目起点为 2.2s、7.123s、0s、9.5s，每轮分别捕获 **30,816、30,816、18,816、18,816** 帧已交付设备的双声道 PCM。全部与独立连续样本参考一致；0s 段覆盖 AAC 起始偏移前的静音，其余段核验非周期左右声道的具体源样本，避免只靠周期音或时长掩盖偏移。队列中的样本可能在定位／取消时被清空，因此这些数字不是实际物理播放总量。

暂停检查在 160ms 前后分别实际查询设备，样本序号递增、QPC 前进，而位置与 padding 保持不变；没有使用缓存状态代替二次查询。overlay 期间音频设备时钟继续前进，子表面保持挂起，9 个画面请求在宿主进入 GPU 前被拒绝，Present 计数不增加。去掉遮挡后同一媒体段恢复普通帧提交。这里的宿主阻塞不是 `DXGI_STATUS_OCCLUDED`，也没有观测到真实 `DXGI_ERROR_WAS_STILL_DRAWING` 或设备丢失。

每轮 51 个回调由 42 次实际 Present 和 9 次宿主拒绝组成；actor 消费 50 个结果，另一个在控制代次退役时清除。sink 保留全部 51 个回调证据，所以没有把未消费结果从真实调用总数中扣除。单槽待处理队列的高水位为 1，本轮未发生队列淘汰，不据此宣称拥堵压力已经覆盖。

取消后旧代次提交返回 `stale`，经过主线程 barrier 再等待 120ms，Present 计数保持不变；两次真实设备查询的位置与 padding 均归零。所有解码进程都记录了回收与退出；主动停止解码的非零退出码如实保留，不改写为完整解码成功。窗口关闭时 DirectComposition 解绑 Commit 成功，子 HWND 失效，窗口类注销，`cleanupBeforeNcDestroy=true`。两轮 `autoCleanupTriggered=false`，因此验证的是 Tauri 正常事件清理路径，不是 WM_DESTROY 兜底或 COM 销毁重入的实测。

## 独立核验与工程检查

[最终独立核验](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/verification-summary-2026-10-17T16-49-21.052Z.json) 为 `verified=true`，两份报告均通过、问题项为零。离线脚本不启动进程或设备，使用独立 JavaScript 像素与 PCM 数学参考，核对原始帧文件、音频时钟、代次、队列守恒、普通帧无读回、遮挡、暂停、尺寸切换和释放记录；50 个源码／程序／前端构建身份在两轮运行前后一致。

运行前身份分别保存于 [首轮构建上下文](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/media-first-pre-run-context.json) 和 [复测构建上下文](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/media-repeat-pre-run-context.json)；正常退出分别保存于 [首轮退出记录](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/media-first-process-exit.json) 和 [复测退出记录](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/media-repeat-process-exit.json)。对象释放、线程结束和进程退出不证明驱动或显存无泄漏。

使用同一程序另行完成[原有 Tauri 校准流程回归](../src-tauri/target/native-tauri-preview-validation/media-regression-6e325fe8-15ae-4871-afdf-9d24dfc2efb2/verification-summary.json)：进程 41092 正常退出，6 个布局／生命周期步骤、5 次校准帧数学验证通过。这是校准路径的独立回归，不计入媒体 42 次 Present、5 次媒体诊断或 PCM 验证数字。

[工程检查记录](../src-tauri/target/native-tauri-media-validation/batch-d4edb7ca-72fc-4069-9330-1fc50a458afb/verification-checks.json) 保存前端 lint、246 项测试及构建通过；默认 Rust 102 项通过、6 项既有专项忽略；启用 `native-preview-host` 的 Rust 256 项通过、8 项忽略；独立呈现例程的 30 项纯测试通过。共享测试不累加为独立覆盖，忽略项不算通过，纯 GPU 几何测试也不替代上面的设备运行。实际 debug 程序构建、格式与 diff 检查均通过，尚未进行 release 性能测量。

工程记录再次核对 50 个源码／程序身份，并检查两次媒体主进程、一次校准回归主进程及 24 个解码子进程均已不存在。它们的退出记录与批次结束后的进程快照分别保留；没有以“查不到进程”补造缺失的正常退出码。

## 复现入口

先完成前端及程序构建并确认退出成功，再启动隔离实例。验证仅允许当前 checkout 的 `src-tauri/target` 下的新输出目录，报告与原始帧文件拒绝覆盖。

```powershell
pnpm build
cargo build --locked --manifest-path src-tauri/Cargo.toml --features native-preview-host,tauri/custom-protocol --bin kaicut
& 'E:\KaiCut（开剪）\src-tauri\target\debug\kaicut.exe' --native-preview-host --mcp --mcp-hidden --mcp-validation --native-media-check-dir 'E:\KaiCut（开剪）\src-tauri\target\native-tauri-media-validation\reproduction' --native-media-fixture-dir 'E:\KaiCut（开剪）\src-tauri\target\native-audio-timing-validation\fixtures-d924c584-e159-44f3-b1d0-678e34c76cd2'
```

这是 debug 隔离入口，默认不启用，也没有接入用户当前项目的播放控件。其他机器需生成并独立核验自己的媒体、WAV 参考和 VFR ledger，再替换路径；不得以显示窗口作为隐藏验收失败的绕过方式。

## 后续范围

只有以上隐藏集成验收通过后，才推进受控的可见预览、实际输入交互、屏幕显示延迟、物理音画同步及长时稳定性。最终播放器还需评估高质量缩放、色彩管理、零拷贝、硬件解码、真实项目多片段衔接和跨设备兼容；这些不由低分辨率夹具的通过结果代替。
