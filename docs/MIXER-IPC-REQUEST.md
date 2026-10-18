# 音频混音器 IPC 实现协议

KaiCut 现在提供音频轨混音器：轨道静音调用 `timeline_edit` 的 `trackState`，音量调用 `trackVolume`，音频片段淡入淡出调用 `clipFade`。所有修改都携带 `projectId + revision`，由 Rust 校验并写入项目历史。预览按同一组参数实时设置 HTML 音频节点音量。

当前协议使用以下项目时间基字段：

```text
volume: number             // 线性增益，0–1，默认 1
fadeInFrames: number       // 非负整数，必须 <= durationFrames
fadeOutFrames: number      // 非负整数，必须 <= durationFrames
```

对应的 `timeline_edit` 操作是 `trackVolume { trackId, volume }` 和 `clipFade { clipId, fadeInFrames, fadeOutFrames }`。`timeline_audio_gain` 提供按播放帧计算后的只读增益，供后续精确播放/导出路径复用。

淡入和淡出总帧数不能超过片段时长；音量范围是 0–1。导出和预览必须继续读取同一组字段，避免出现“界面显示已调节、实际声音未变化”的假功能。
