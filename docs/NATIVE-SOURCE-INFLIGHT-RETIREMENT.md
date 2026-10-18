# 原生源预览在途提交退役

这批验证覆盖真实 `present_media` 已返回、结果尚未发布到原生队列时关闭或定位素材的顺序。它只在隐藏、隔离、debug 验证入口启用一次性停留点；默认播放器、项目格式和公开命令不变。

## 验证方式

原生提交返回后，主线程回调在发布 `MediaAck` 前短暂停留，保存真实帧身份、Present 结果和 sink 状态。验证线程通过现有 `native_source_control` 命令包装器发起关闭或定位，不调用 DOM、截图或桌面输入。控制必须在停留期间保持未完成；关闭/定位先退役队列，再显式释放停留点，最后等待同一命令完成。

| 场景 | 退役条件 | 迟到结果 |
| --- | --- | --- |
| `inflight-cancel` | sink 从未关闭变为关闭，`inFlight` 仍为真，关闭命令仍未返回 | `Closed`，不发布旧 ACK；随后切换 B 并播放超过 300ms |
| `inflight-seek` | render epoch 增加、sink 未关闭，`inFlight` 仍为真，定位命令仍未返回 | `Stale`，不发布旧 ACK；同一素材在 3 秒新 epoch 准备后播放超过 3.3 秒 |

## 实际结果

2026-10-18，四个独立隐藏实例通过：`inflight-cancel` 两轮、`inflight-seek` 两轮。受控停留耗时为 6、7、11、25 ms；这是调度窗口，不是用户取消延迟，也不是驱动调用截止时间。四轮都观察到真实 `Present` 返回、匹配的 session/surface/frame 身份和未完成的原生控制，之后才释放停留点。独立验收报告为 [inflight-verification-98a563db-e69f-4381-87d2-14b108d6cfc3.json](../src-tauri/target/native-source-validation/inflight-981ce3e6-0640-4f4d-a94f-a7ef52da9528/inflight-verification-98a563db-e69f-4381-87d2-14b108d6cfc3.json)，`ok=true / complete=true`。

独立校验拒绝 154 项篡改证据，确认 6 个 actor 清理、24 个解码子进程和 18 个 profile 探测进程均 join/reap；四轮应用退出码均为 0。运行前源码、程序、前端产物和 sidecar 共 67 项身份一致，`kaicut.exe` SHA-256 为 `af0862ce149c73c404749ce7db9334b2ba1c0b1decf21bd329b915f7fd30f233`。最终 PID/创建时间采样记录 75 个自有 PID，当前存活数为 0。

同批次的源控件回归退出码为 0；旧媒体路径单轮受影响回归也通过（42 次 Present、5 次准备画面读回、12 个解码子进程）。旧媒体校验器忽略新增的 `queueCompletion` 诊断字段，并单独校验队列分类；证据保存在 `src-tauri/target/native-tauri-media-validation/inflight-regression-5c714fc6-af17-4b9a-ab35-334bda91ae31/single-regression-verification-05f8ec01-5fba-4667-a87a-9266fc7e11de.json`。

## 限制

- 退役发生在系统 Present 调用已经返回之后；不能证明能中断卡住的显卡驱动调用，也不能撤回已进入的 Present。
- 没有进行可见屏幕像素、实际听感或物理音画同步验收；隐藏窗口报告中的 `screenDisplayed=false` 仍然有效。
- 失败路径中，若验证停留点已经 arm 但尚未被某一帧认领，析构只会尝试释放已认领的停留点，未认领状态依赖十秒边界收束。该入口仅用于隔离验证，不影响默认运行。
- 采样可能遗漏短寿命子进程；回收结论同时依赖原始 decoder/probe join/reap 台账。WebView 仍有 `Chrome_WidgetWin_0` 注销警告 1412，未宣称已解决。

下一步是覆盖真实 Stop/Reset/解绑失败和设备拔插，再评估是否把原生单源路径扩大到更多素材；默认 WebView 播放器继续保持不变。
