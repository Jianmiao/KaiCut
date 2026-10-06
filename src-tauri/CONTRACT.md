# Native command contract

The Rust session is the authoritative project state. Frontend calls these Tauri commands with `invoke` and replaces its project projection with the returned session. All project commands serialize access to the session. Native pickers and confirmations execute outside the async runtime worker.

## Shared result

Project commands return `{ project, path: string | null, dirty: boolean, status: string }`. `project` uses schema version 1 from `src/types/project.ts`; each media reference may additionally have `sourcePath?: string`, preserving the native absolute file path. Media bytes, object URLs, thumbnails, and clips are not persisted in this scaffold. New projects contain two empty video tracks and two empty audio tracks. `createdAt` and `updatedAt` are RFC 3339 UTC strings.

Errors reject the Tauri command with a user-readable string. Cancelling a picker or declining a discard confirmation resolves to the current session with a cancellation status; it does not replace the project, clear dirty state, or change the path.

| Command | Arguments | Behavior |
|---|---|---|
| `project_get` | none | Return current session. |
| `project_new` | none | Confirm discard if dirty, then create an untitled project. |
| `project_open` | none | Confirm discard if dirty, open a native `.kaicut` picker, validate the complete JSON before replacing the session. |
| `project_save` | none | First save uses a native `.kaicut` picker; subsequent saves reuse the current path. Only successful durable replacement clears dirty state. |
| `project_rename` | `{ name: string }` | Trim and validate the project name; mark changed state dirty. |
| `project_import` | none | Native multi-file picker for mp4/mov/mp3/wav/png/jpg/jpeg. Add file metadata and source paths; no probing or decoding yet. |
| `media_engine_status` | none | Return `{ available: boolean, version: string | null, message: string }` after checking only packaged/sibling FFmpeg paths. Does not search PATH or download binaries. |

Opening/newing and closing the native window require an explicit native discard confirmation whenever the session is dirty. Closing is cancelled while a project operation is active.

## Persistence limits

- Single `.kaicut` JSON file, at most 10 MiB; references stay external.
- Schema version 1, validated timestamps/timebase/names/IDs/media types, at most 2 video + 2 audio tracks with empty clip lists.
- Reject unsupported schema versions, unknown persisted fields, duplicate identifiers, invalid metadata, and nonempty clips instead of silently dropping information.
- Save via a temporary file in the destination directory, flush/sync, then replace the destination; failed writes leave the prior file and session intact.
- Import preserves selected files and reads metadata only. Duplicate paths are skipped.

## Sidecar packaging

The default development build deliberately does not require a missing FFmpeg binary. The optional `tauri.sidecar.conf.json` enables `binaries/ffmpeg` as an external binary once audited LGPL platform builds have been supplied (default LGPL-2.1-or-later, GPL/nonfree excluded). Availability/version is not a license verification. KaiCut itself is Apache-2.0. See `binaries/README.md`.
