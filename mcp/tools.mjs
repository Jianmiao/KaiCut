// Keep this allowlist in step with src-tauri/src/mcp_bridge.rs and CONTRACT.md.
const id = { type: 'string', minLength: 1, maxLength: 256 }
const frame = { type: 'integer', minimum: 0, maximum: Number.MAX_SAFE_INTEGER }
const object = (properties) => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false })
const ids = (minItems = 1) => ({ type: 'array', items: id, minItems, maxItems: 10000, uniqueItems: true })
const session = { projectId: id, revision: frame }
const localPath = { type: 'string', minLength: 1, maxLength: 32768, pattern: '^(?:[A-Za-z]:[\\\\/]|/|\\\\\\\\)' }
const projectPath = { ...localPath, allOf: [{ pattern: '\\.[kK][aA][iI][cC][uU][tT]$' }] }
const outputPath = { ...localPath, allOf: [{ pattern: '\\.[mM][pP]4$' }] }
const jobId = { type: 'string', minLength: 1, maxLength: 80, pattern: '^[a-z0-9-]+$' }
const tagged = (type, properties) => object({ type: { const: type, type: 'string' }, ...properties })

const edit = {
  oneOf: [
    tagged('add', { mediaId: id, trackId: id }),
    tagged('split', { clipId: id, frame }),
    tagged('delete', { clipIds: ids() }),
    tagged('rippleDelete', { clipIds: ids() }),
    tagged('duplicate', { clipId: id }),
    tagged('move', { clipId: id, trackId: id, startFrame: frame }),
    tagged('moveMany', { moves: { type: 'array', minItems: 1, maxItems: 10000, items: object({ clipId: id, trackId: id, startFrame: frame }) } }),
    tagged('trim', { clipId: id, sourceInFrame: frame, startFrame: frame, durationFrames: { ...frame, minimum: 1 } }),
    tagged('trackState', { trackId: id, locked: { type: 'boolean' }, muted: { type: 'boolean' } }),
    tagged('trackVolume', { trackId: id, volume: { type: 'number', minimum: 0, maximum: 1 } }),
    tagged('clipFade', { clipId: id, fadeInFrames: frame, fadeOutFrames: frame }),
  ],
}

const tool = (name, description, properties = {}, readOnlyHint = true, destructiveHint = false) => ({
  name,
  description,
  inputSchema: object(properties),
  annotations: { readOnlyHint, destructiveHint, idempotentHint: readOnlyHint, openWorldHint: false },
})

export const toolDefinitions = [
  tool('get_state', 'Read the live Rust-authoritative KaiCut project, timeline, revision and history availability. Read this before editing. Does not launch KaiCut.'),
  tool('project_open_path', 'Open an existing local .kaicut project by absolute path. Requires the current projectId/revision; refuses to discard an unsaved dirty project. Resets editing view/history even when reopening the same project.', { ...session, path: projectPath }, false, true),
  tool('project_import_paths', 'Import 1–100 explicit local media paths into the current project; source files are never changed. Requires current projectId/revision. Does not open a native file picker.', { ...session, paths: { type: 'array', items: localPath, minItems: 1, maxItems: 100, uniqueItems: true } }, false),
  tool('project_save_as', 'Save the current project to a new absolute .kaicut path. Never overwrites an existing file. Establishes the saved project path and clears dirty state. Requires current projectId/revision.', { ...session, path: projectPath }, false),
  tool('ui_inspect', 'Inspect the running UI projection: playhead, selection, playback, panel sizes and visible controls. Structured state only; this does not capture pixels or prove visual appearance.'),
  tool('media_check_links', 'Check whether the current project media references still point to readable local files.'),
  tool('media_probe', 'Probe current project media metadata with the packaged FFmpeg sidecar; may take up to 60 seconds. Does not import or modify source files.'),
  tool('export_job_list', 'List export jobs known to the running app. Does not start or cancel encoding.'),
  tool('export_job_status', 'Read one existing export job by its jobId. Check this after ambiguous start/cancel timeouts.', { jobId }),
  tool('export_preflight', 'Read-only export checks for the current projectId/revision and a new absolute .mp4 outputPath. Does not create a job or start encoding. Allow up to 60 seconds for native checks.', { ...session, outputPath }),
  tool('export_prepare', 'Freshly preflight in Rust and create one queued job with an immutable project snapshot. Requires current projectId/revision and a new absolute .mp4 outputPath. Does not start encoding and never overwrites files. Allow up to 60 seconds. After timeout or request cancellation, use export_job_list before considering retry: a queued job may already exist.', { ...session, outputPath }, false),
  tool('export_job_start', 'Start or retry one known queued/failed/cancelled immutable jobId using its saved snapshot, not the current project. Cannot override output paths or snapshot data. A timed-out/cancelled tool request does not cancel the export job; check export_job_status before retrying.', { jobId }, false),
  tool('export_job_cancel', 'Cancel one known export jobId through the export worker and its cleanup process. This is the explicit job-cancellation operation; cancelling the MCP request alone does not cancel the job. If the request times out, check export_job_status before retrying.', { jobId }, false, true),
  tool('timeline_edit', 'Apply one undoable edit atomically. Use projectId and revision returned by get_state; stale revisions are rejected. delete/rippleDelete remove timeline clips, never source files. Re-read state after ambiguous timeouts; never blindly retry a mutation.', { ...session, edit }, false, true),
  tool('timeline_undo', 'Undo one timeline edit using the current projectId and revision. Re-read state after the operation.', session, false, true),
  tool('timeline_redo', 'Redo one timeline edit using the current projectId and revision. Re-read state after the operation.', session, false, true),
  tool('ui_seek', 'Move the preview playhead to a frame of the current project. Does not modify project content.', { ...session, frame }, false),
  tool('ui_select', 'Select timeline clips, or clear the selection with an empty clipIds array. Does not modify project content.', { ...session, clipIds: ids(0) }, false),
  tool('ui_play', 'Start preview playback in the running app. Requires current projectId and revision.', session, false),
  tool('ui_pause', 'Pause preview playback in the running app. Requires current projectId and revision.', session, false),
]
