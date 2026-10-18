# 原生源预览跨度压力验证

## 范围

本批验证 Windows 开发开关 `--native-preview-host --native-source-preview` 下的单源原生预览控制路径。实例均使用 `--mcp --mcp-hidden --mcp-validation`，窗口保持隐藏且不获取焦点，音频只进入本实例的静音 WASAPI 会话；没有桌面输入、可见画面、真实听感或物理音画同步验收。

固定 A/B 视频夹具为 320×180、30fps、H.264，音频为 48kHz 双声道 AAC。独立原始 PTS 和采样参考确认 600 个视频帧、938 个 AAC 帧、960512 个采样，归一化结束时间为 **20,010,666,666ns**。夹具清单 SHA-256 为 `48b927e6bfa7d15b5d6b10ee9c1bcff56f56b5cfb93e9e3f4a927c7e9efab0d7`。

## 固定步骤

每轮只执行一次固定序列：

1. 选择素材并等待 Ready。
2. 播放至约 5 秒后暂停。
3. 保持暂停 150ms，比较缓存状态和 pump count。
4. 恢复播放并自然播放到 EOF。
5. 定位到 10 秒，播放约 1 秒后暂停。
6. 定位到 1 秒，播放约 1 秒后暂停。
7. 定位到片尾前 200ms，播放到 EOF。

共记录 14 个步骤。每段 Start 账本要求与 source epoch 对齐：第一段两次 Start 均为 epoch 1，之后三段分别为 epoch 2、3、4。关键生命周期账本最多保留 64 项；视频事件最多保留 128 项且是滚动记录，不能被解释为完整逐帧记录。

## 结果

正式批次：`src-tauri/target/native-source-validation/span-stress-final-20261018-r3`。

独立批次校验报告：`span-stress-verification-4c7ee44d-cf1d-4a16-b2b7-4f16a602b1d5.json`，SHA-256 为 `d45bcd833c5587f60bf64896d46d8e320058e92822ec061fb13db30b5634835d`。校验器 SHA-256 为 `1e44d93f05c91f20fe1b30cdbbb6f8b68fc3affec017a11132063fa70b9df470`。

| 轮次 | 应用 PID | 解码进程 | Profile 进程 | 采样子进程 | 自然欠载 | 原始报告 SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| span-1 | 13964 | 12 | 3 | 26 | 0 | `9c90a3a1187251d1725e30153fc8e9755eda9f65976600d316a2a7140eccd12d` |
| span-2 | 23484 | 12 | 3 | 31 | 0 | `1c6e0cadac8036c38fe4e4ffac80e4cd560db1084c009f276c678fcb59090617` |

两轮的程序/源码/前端/sidecar 身份摘要均为 `875a40e78501b2bc3a8a4b19d4e4c67ee17406cdcef113863b0542ba7c36fb74`，调试可执行文件 SHA-256 均为 `9cf6f36e35f24f80f75a4f5abbf2154396ebecdc73159618ce71ba9ef8a5e6ce`。每轮均有退出码 0、decoder join/reap 和窗口清理证据；批次校验拒绝 34 项篡改输入（每轮 17 项）。结束后的进程检查没有发现 KaiCut、ffmpeg 或 ffprobe 残留进程。

跨度验证后的旧媒体验证也通过两轮；独立结果见 `src-tauri/target/native-tauri-media-validation/span-regression-r3/verification-summary-2026-10-17T22-58-42.396Z.json`（SHA-256 `f5d306b09a045ee1e8262fed0bc159f24724c2af812ec3097b05e72b6d36f96d`）。普通素材控件回归另完成一轮9步骤播放/暂停/定位/重播/切源验证，独立报告 `src-tauri/target/native-source-validation/span-controls-r3/source-verification-f584acf2-5097-48f3-a433-f5dd64e06754.json`（SHA-256 `9e16a24471ce5aa1c31b096ab6a568ea75f7c2df8d29c70a90a5e4725ae1d850`）。完整命令、前端/Rust检查、程序身份和进程证据汇总在本批 `verification-checks.json`。

## 判断与限制

本机固定 20 秒夹具的两轮跨度序列共观察到 0 次自然欠载。这个结果与三轮正常控件和两轮延迟阳性对照的诊断结果一致，但不能删除此前的自然欠载失败记录，也不能证明自然欠载根因已经修复。150ms 观察只比较暂停后的缓存状态和 pump count，没有读取新鲜设备时钟，不能作为硬件冻结或实时保证。

本批不证明可见画面、扬声器听感、物理音画同步、长素材资格、其他设备稳定性、真实驱动故障处理或默认播放器已切换。后续 [59 秒跨度压力验证](./NATIVE-SOURCE-LONG-SPAN-STRESS.md)已在最终构建上完成两轮：中点暂停/恢复至 EOF、7.5 秒与 30 秒定位及片尾播放，累计自然欠载为 0。它仍处于现有 60 秒准入范围内；下一步扩大分辨率、素材和设备矩阵，继续保留发生点诊断，不降低验收阈值。
