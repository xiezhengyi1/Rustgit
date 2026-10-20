// Shapes of the `rgit --json` interface, version 1 (docs/gui-interface.md).

export const JSON_INTERFACE_VERSION = 1;

export type ObjectId = string;

export interface VersionData {
  cli: string;
  json_interface: number;
  format: number;
}

export type ChangeKind = "added" | "modified" | "deleted" | "renamed";

export interface Change {
  path: string;
  kind: ChangeKind;
  from?: string;
}

export interface Commit {
  id: ObjectId;
  short: string;
  parents: ObjectId[];
  author_name: string;
  author_email: string;
  /** Unix seconds */
  timestamp: number;
  tz_offset_min: number;
  message: string;
  branches: string[];
}

export interface MergeState {
  kind: string;
  label: string;
}

export interface ConflictEntry {
  path: string;
  kind: string;
  resolved: boolean;
}

export interface StatusData {
  branch: string;
  head: ObjectId | null;
  changes: Change[];
  skipped: { path: string; reason: string }[];
  merge: MergeState | null;
  conflicts: ConflictEntry[];
  ahead?: number;
  behind?: number;
}

export interface InitData {
  uuid: string;
  root: string;
}

export interface CommitData {
  id: ObjectId;
  short: string;
  branch: string;
}

export interface LogData {
  commits: Commit[];
}

export interface ShowData {
  commit: Commit;
  changes: Change[];
  diff: string;
}

export interface DiffData {
  changes: Change[];
  diff: string;
}

export interface BranchInfo {
  name: string;
  id: ObjectId;
  short: string;
}

export interface BranchListData {
  current: string;
  branches: BranchInfo[];
}

export interface BranchRefData {
  name: string;
  id: ObjectId;
}

export interface SwitchData {
  branch: string;
}

/** restore / revert / merge / unshelve */
export interface MergeLikeData {
  commit: ObjectId | null;
  fast_forward: boolean;
  already_up_to_date: boolean;
  conflicts: string[];
  restored?: string[];
}

export interface ResolveData {
  path: string;
}

export interface ShelfEntry {
  /** short name used by unshelve / shelf-drop, e.g. "1" */
  name: string;
  /** full ref, e.g. "shelf/1" (display only) */
  ref: string;
  id: ObjectId;
  message: string;
}

export interface ShelvesData {
  shelves: ShelfEntry[];
  recovery: unknown[];
}

export interface StatsData {
  commits: number;
  branches: number;
  objects: number;
  logical_bytes: number;
  stored_bytes: number;
  delta_objects: number;
  pack_files: number;
  pack_bytes: number;
  index_bytes: number;
  repo_bytes: number;
}

export interface VerifyData {
  objects_checked: number;
  commits: number;
  problems: string[];
}

export interface GcData {
  moved: number;
  dropped: number;
  freed_bytes: number;
  repo_bytes_before: number;
  repo_bytes_after: number;
}

export interface ReindexData {
  objects: number;
  packs: number;
  problems: unknown[];
}

export interface RemoteData {
  remotes: { name: string; url: string; secure?: boolean; host_key?: string | null }[];
}

export interface RefUpdate {
  ref: string;
  old?: string | null;
  new: string | null;
  generation?: number;
}

export interface TransferStats {
  logical_bytes: number;
  objects_already_complete: number;
  objects_resumed: number;
  objects_transferred: number;
  wire_bytes_received: number;
  wire_bytes_sent: number;
}

export interface PushData {
  connection?: ConnectionInfo;
  already_applied: boolean;
  up_to_date: boolean;
  stats: TransferStats;
  updated: RefUpdate[];
}

export interface FetchData {
  connection?: ConnectionInfo;
  /** how the fetched branch relates to yours, e.g. "diverged" */
  state: string;
  up_to_date: boolean;
  stats: TransferStats;
  updated: RefUpdate[];
}

export interface PullData {
  fetch: FetchData;
  /** null when there was nothing to merge */
  merge: MergeLikeData | null;
}

export interface CloneData {
  connection?: ConnectionInfo;
  branch: string;
  dest: string;
  head: ObjectId | null;
}

export interface ProgressEvent {
  event: "progress";
  /** e.g. negotiating, uploading, downloading */
  phase: string;
  done: number;
  total: number;
  bytes?: number;
}

/** Any NDJSON line with an "event" key: progress lines, `listening`, `connection`, ... */
export interface EngineEvent {
  event: string;
  [key: string]: unknown;
}

export interface ErrorBody {
  code: string;
  message: string;
  paths?: string[];
}

export interface HostEvent extends EngineEvent {
  addr?: string;
  project?: string;
}

export interface ConnectionInfo {
  state: "direct" | "relayed" | "unreachable" | "connecting";
  path?: string | null;
  error?: string | null;
  used_relay?: boolean;
  used_direct?: boolean;
}
export interface DeviceIdentity { fingerprint: string; key_storage: string; key_storage_protected: boolean }
export interface Invitation { id: string; label: string; role: string; status: string; expires: number; uses: number; max_uses: number }
export interface EnrolledDevice { fingerprint: string; name: string; role: string; status: string; last_seen: number | null }
export interface DiscoveredHost { instance: string; urls: string[]; reachable: boolean | null; verified: false }
export interface JoinData { dest: string; device_fingerprint: string; host_fingerprint: string; role: string; connection?: ConnectionInfo }
