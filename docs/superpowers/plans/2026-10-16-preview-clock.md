# KaiCut Preview Clock Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用真实 WebView 媒体音频时钟驱动项目预览，减少重复 seek，并取得不少于 600 秒播放及 seek/pause/切段的可复查证据。

**Architecture:** 把源时间映射、连续区间判定和同步策略放进 `preview-clock.ts` 纯模块，由预览组件的实际媒体 adapter 调用。项目时钟优先独立音轨，其次可见视频，无媒体区间才由单调时钟推进；当前不假设已存在 Rust/native 解码器。Rust 仍是项目/历史/revision 权威状态，媒体观测与播放状态是运行时数据。

**Tech Stack:** Tauri 2、Windows WebView2、React/TypeScript、Zustand、Vitest、既有 MCP stdio 工具与 LGPL FFmpeg/ffprobe 合成素材生成。

**Spec:** [ARCHITECTURE.md](../../ARCHITECTURE.md#已批准的渐进内核第一批)、[PREVIEW-PARITY.md](../../PREVIEW-PARITY.md#已批准的下一批音频主时钟)、[TASKS.md](../../TASKS.md#m2时间线与预览核心)。

## Global Constraints

- 用户已批准方案二“渐进内核”及本批交付，直接实施与验证；不另起设计审批。
- 当前以 Windows 为实现与验收平台；macOS 待设备，Linux 延后。
- 保持 Rust 项目权威状态、现有 `.kaicut v2`、FFmpeg 二进制与导出 renderer 不变。
- 无 UI 布局、样式、图标、动效或快捷键改动；不新增 AI、云服务或完整 native 播放器模块。
- 不控制用户桌面；真实测试使用自有隔离、隐藏、不获取焦点的 Tauri 实例和合成媒体。
- 音频主时钟指 HTMLMediaElement 的 currentTime，不等同 WASAPI/CoreAudio 的物理播放位置。
- 原始媒体不写入项目文件；所有生成素材、测试项目和报告在本仓库 `src-tauri/target` 的新运行目录中。
- 完成后必须实际运行 lint/test/build；修改 Rust 时另跑 cargo test/fmt。报告只记录实际命令结果与证据。

## 文件职责与协作边界

| 文件 | 职责 |
| --- | --- |
| 新增 `src/lib/preview-clock.ts` 与 `.test.ts` | 小型纯模块：时钟换算、同源连续性、主从同步策略与边界测试 |
| 修改 `src/components/editor/preview-area.tsx` 与 `.test.tsx` | 使用实际媒体节点读回，编排主时钟、缓冲、seek、pause、play Promise 和节点复用 |
| 修改 `src/hooks/use-mcp-bridge.ts` 与测试 | 从实际节点报告时钟来源、缓冲、node ID、playbackRate 和硬 seek 计数，复用原生隐藏/焦点读回 |
| 新增 `mcp/playback-check.mjs`、修改 `package.json` | 600 秒实际运行和边界验收，不替换原有短性能基线脚本 |
| 本计划、ARCHITECTURE/TASKS/PREVIEW-PARITY | 记录设计约束与验收目标，不预填成功计数 |

不按未来 adapter 数量设计泛化类层级；一个纯模块可以承载相关规则。高频运行时观测局部保存，Zustand 仅接收展示所需的播放头/模式投影。跨层函数接口保持如下名字和类型，以防计划与实现分叉。

## 执行状态（长时验收尚未完成）

时钟/adapter 实现、确定性回归、30 秒真实 smoke 及 14 秒边界小试已执行；增加无前进 watchdog 后，前端测试、lint、build 和桌面调试打包已重新通过。这里只勾选已经执行的步骤，不把小试计为 600 秒验收。

第一轮 600 秒运行约在 277 秒时，由操作者因独立审查发现 watchdog 缺失而主动暂停；该次不算通过，也不标为自发播放故障。修复后已重新启动完整 600 秒验证，本计划保留长时完成与最终报告为未完成，结果以对应独立报告为准。

## Task 1: 时钟映射与同步策略

**Files:** `src/lib/preview-clock.ts`、`src/lib/preview-clock.test.ts`。

**Interfaces:** 使用既有 `TimelineClip`；三个纯函数同时供测试和真实 adapter 调用：

```typescript
mediaProjectFrame(clip: TimelineClip, seconds: number, fps: number): number
continuousSource(previous: TimelineClip | undefined, next: TimelineClip | undefined): boolean
syncAdjustment(current: number, desired: number, state: {
  playing: boolean; master: boolean; seeking: boolean; force: boolean
}): { rate: number; seek?: number }
```

- [x] 写源入点/非整数 fps、相邻片段、非连续源入点测试。测试例：start=30、sourceIn=15、seconds=1.01、fps=30 应得到项目位置45.3；同片段 seconds=1.001、fps=30000/1001 应得到45，禁止内部提前取整。
- [x] 写同步策略测试：seeking 时不重复定位；master 正常播放保持1x；paused/force 在偏差>1ms时给出 seek；slave 偏差≤20ms保持1x、20–250ms有限调速、>250ms才硬定位。现有 seek guard 优先于force。

```typescript
expect(syncAdjustment(1, 1.1, { playing: true, master: false, seeking: false, force: false }))
  .toEqual({ rate: 1.03 })
expect(syncAdjustment(1, 4, { playing: true, master: false, seeking: true, force: true }))
  .toEqual({ rate: 1 })
expect(syncAdjustment(1, 4, { playing: true, master: true, seeking: false, force: false }))
  .toEqual({ rate: 1 })
```

- [x] 运行 `pnpm test src/lib/preview-clock.test.ts` 并验证边界；最终前端回归包含该文件。
- [x] 实现精确映射和纯决策，策略不直接调用 DOM、不读全局 performance、不访问项目 store。

```typescript
const projectFrame = clip.startFrame + seconds * fps - clip.sourceInFrame
const drift = desired - current
const slaveRate = Math.max(0.97, Math.min(1.03, 1 + drift * 0.3))
```

- [x] 重跑定向测试；核对连续性同时要求相同 mediaId、相邻项目区间和相邻源区间，不只检查文件名。

**本任务可独立审核：**纯接口返回值可复现；没有播放节点、平台能力或 UI 依赖。

## Task 2: 接入真实 WebView 媒体与主时钟

**Files:** `src/components/editor/preview-area.tsx`、`preview-area.test.tsx`；复用现有 `timeline-preview.ts` 的 `resolveTimelineFrame`，不改变导出合成规则。

**Interfaces:** 每个活动媒体节点注册实际 element、同步执行入口与等待状态；内部形状如下，保持在预览实现内：

```typescript
interface RegisteredMedia {
  element: HTMLMediaElement
  sync: (frame: number, hold?: boolean) => void
  blocked: () => boolean
}
```

- [x] 写组件测试：独立音频 currentTime 推进而 performance推进不同，项目帧跟随音频；两条独立音轨按项目轨序选主，muted 排除、gain0保留。
- [x] 写无音频视频主时钟、图片区间/黑场单调时钟、readyState<3/seeking/waiting/节点尚未挂载时冻结的测试；冻结时暂停从节点，恢复后不追赶墙钟。
- [x] 补充并执行无前进 watchdog 回归：主节点无 waiting 事件、readyState 正常但 currentTime 连续 250ms 不前进时 hold 从节点；等待中不重复硬 seek，主媒体恢复推进后从节点恢复；允许 100ms 量化时间更新，显式 seek 重置旧观察。
- [x] 实现确定性选主和坐标映射。已有主媒体未就绪不能改用墙钟绕过；只有确实没有活动媒体的区间可单调推进。

```typescript
const owner = current.audio[0]
  ?? (current.visual?.asset?.kind === 'video' ? current.visual : undefined)
// owner exists but is missing, waiting, seeking or not ready => hold position.
// owner ready => mediaProjectFrame(owner.clip, element.currentTime, fps).
// no owner => previousFrame + elapsedMonotonicSeconds * fps.
```

- [x] 将下一轨道/片段边界作为推进上限；慢 RAF 不可跳过尚未挂载过的整个媒体区间。正常媒体ended时检查源范围，提前结束要报告错误，不能永远卡住。
- [x] 写并验证 seek/pause 生命周期测试：显式 seek 用独立 epoch 重锚；普通时钟写播放头不触发seek；暂停/项目/模式/revision变化清理旧流；晚到的 Promise 不恢复旧播放。
- [x] 用轨道＋媒体＋来源指纹保持同源节点 key；测试连续分割后 node ID 不变且没有额外硬seek，同源跳源入点强制定位，换素材换节点。DOM `clipId`仍更新，不能因node复用保留旧选择/媒体映射。
- [x] 执行预览、clock 与帧解析定向/全量测试，覆盖已有上下轨遮挡、音频尾段、离线源错误与项目切换。

**本任务可独立审核：**实际 adapter 使用纯策略；没有新增持久化字段，没有逐帧 IPC，没有不可测试的第二份项目状态。

## Task 3: 观测与真实长时验收

**Files:** `src/hooks/use-mcp-bridge.ts` 与测试、`mcp/playback-check.mjs`、`package.json`。

**Interfaces:** 复用既有 `ui_inspect/ui_play/ui_pause/ui_seek`，返回真实 `nativeWindow.visible/focused` 与媒体 node ID、currentTime、expectedSourceTime、driftSeconds、readyState/seeking/paused/error、playbackRate、硬seek计数，以及预览主钟类型/clipId/buffering。字段是运行时诊断，不能进入 `.kaicut`。

- [x] 用组件测试证明检查结果来自实际媒体元素与运行时状态；不可把请求参数原样当作成功证据。
- [x] 建立 `pnpm mcp:playback-check`，默认同时要求至少600秒真实wall时间与600秒项目推进；`--seconds 30`仅用于smoke，结果不可叫“10分钟验收”。生成两个610秒合成源；`--fixtures`只读复用前次生成目录，验证指纹。
- [x] 输出目录解析后必须仍在本仓库target内，每次新建run目录；连接全新干净空项目的隔离hidden/validation实例。所有ui调用验证主窗口隐藏且未获焦点；不得操纵其它窗口修复失败条件。
- [x] 快速确认脚本/API链路，完成30秒真实smoke：

```powershell
pnpm mcp:playback-check --connection-file '<隔离连接文件>' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\mcp-playback-validation' --seconds 30
```

- [x] 在smoke暴露的问题修复后运行完整600秒，不通过加速播放、跳帧或缩短实际等待冒充：

```powershell
pnpm mcp:playback-check --connection-file '<新的隔离连接文件>' --output-dir 'E:\KaiCut（开剪）\src-tauri\target\mcp-playback-validation'
```

- [x] 在smoke中执行pause350ms、四次seek及14秒/420帧边界小试：0–4秒同源连续剪切，4–6秒同节点相邻不连续源入点，6–8秒gap，8–10秒源入点跳跃，10–12秒仅音频，12–14秒仅视频。确认连续剪切不增加硬seek；相邻跳源保持node ID但增加定位计数。
- [x] 完整600秒运行结束后重新执行上述pause、seek与14秒边界检查；smoke结果不能替代此步骤。
- [x] 脚本独立比较音频和视频映射后的项目位置，保留全部与稳定样本、P50/P95/max/首尾、稳定样本覆盖率及排除原因。30fps一帧=1/30秒，门槛要求稳定样本P95和max均小于一帧，不放宽为旧150ms工程门槛。
- [x] 在完整600秒结果上审查上述严格门槛，并记录是否满足；不因时钟架构变化直接宣称达标。
- [x] 写 `playback-validation.json`，包括真实经过时间、项目推进量、时钟差、硬seek/速率、节点复用、buffering、环境和媒体指纹。失败也保存报告，并明确物理音频延迟、可见frame pacing、听感/VFR/HDR未验证。

**本任务可独立审核：**600秒是真实持续播放证据；不凭按钮应答、素材时长或模型计算结果认定成功。

## Task 4: 整体验证与交付记录

**Files:**上述改动及最终独立验收报告；本计划和架构文档只写设计/范围，不预写通过。

- [x] 运行 `pnpm lint`、`pnpm test`、`pnpm build`，watchdog修复后重新通过；不在计划中预填最终验收计数。
- [x] 汇总本轮 `pnpm test:mcp` 与最终 `git diff --check` 结果，逐项检查退出码及完整输出。
- [x] 核对本批未改Rust业务实现，不借用旧cargo结果冒充新执行；未来若触及Rust，额外运行 `cargo test --locked --manifest-path src-tauri/Cargo.toml` 与 `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`。
- [x] 执行 `pnpm tauri:build --debug --config src-tauri/tauri.sidecar.conf.json`，watchdog修复后调试打包通过；产物/程序哈希随最终验收报告统一记录。
- [x] 将真实运行命令、样本时长/数量、时钟来源与误差、旧基线对照、失败和限制写入独立报告；保留未满足的一帧或边界门槛。暂停并关闭自有测试实例前确认没有在导出的任务，不影响用户其它应用。
- [ ] 评审本批结果后决定native探针：Windows WASAPI时间位置、Media Foundation解码与D3D呈现、seek完成/取消/重建的最小链路先独立验证，再接现有adapter位置。不在本批创建伪native实现或更换FFmpeg。

## 计划自检

- 三个纯函数的参数/返回值与当前实现协作约定一致；无虚构的第二个policy模块或不存在的通用播放器类。
- Rust项目权威、schema、sidecar、导出、Windows优先、无UI/桌面控制约束均贯穿四个任务。
- 长时、同步、seek、pause、连续/跳源切段、音频/视频单独区间、缓冲与节点复用都有明确检查。
- 物理输出与WebView观测分开；历史65秒结果保持历史，不用本计划预先关闭M2。

## 本轮结果

221项前端、20项MCP协议测试及lint/build/Windows打包通过。最终600.395秒实际播放、1144样本，稳定相对时钟差P95 9.910ms、最大16.465ms；14秒边界功能及既有工作流回归通过。开播/跳源瞬时偏差及切换等待仍保留，未关闭完整M2门槛。完整数据、产物、主动作废基线及限制见 [AUDIO-CLOCK-VALIDATION.md](../../AUDIO-CLOCK-VALIDATION.md)。native完整链路探针作为后续任务，当前不声称完成。
