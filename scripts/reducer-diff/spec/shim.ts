// Runs act_core beside the TS reducer and projections inside the TS test suite (see spec.sh): one
// reduce_lines and one view_lines child per test worker, spoken to through FIFOs.
import { execFileSync, spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { expect } from "vitest";

const binary = process.env.ACT_RUST_BIN!;
type Pipes = { write: number; read: number; pending: string };
const opened = new Map<string, Pipes>();

function open(binary: string): Pipes {
  const held = opened.get(binary);
  if (held) return held;
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "act-rr-"));
  const requests = path.join(dir, "req");
  const answers = path.join(dir, "ans");
  execFileSync("mkfifo", [requests, answers]);
  const child = spawn(binary, [requests, answers], { stdio: ["ignore", "ignore", "inherit"], detached: false });
  child.unref();
  const write = fs.openSync(requests, "w");
  const read = fs.openSync(answers, "r");
  process.on("exit", () => { try { fs.closeSync(write); child.kill(); fs.rmSync(dir, { recursive: true, force: true }); } catch {} });
  const pipes = { write, read, pending: "" };
  opened.set(binary, pipes);
  return pipes;
}

function ask(value: unknown, program = binary): any {
  const p = open(program);
  fs.writeSync(p.write, JSON.stringify(value, plain) + "\n");
  const buffer = Buffer.alloc(1 << 20);
  for (;;) {
    const newline = p.pending.indexOf("\n");
    if (newline >= 0) {
      const line = p.pending.slice(0, newline);
      p.pending = p.pending.slice(newline + 1);
      return JSON.parse(line);
    }
    const n = fs.readSync(p.read, buffer, 0, buffer.length, null);
    if (n === 0) throw new Error("reduce_lines closed");
    p.pending += buffer.toString("utf8", 0, n);
  }
}

const plain = (_key: string, value: unknown) => value instanceof Set ? [...value] : value instanceof Map ? Object.fromEntries(value) : value;
const PLACEHOLDER = /00000000-0000-4000-8000-(\d{12})/g;

function replaceIds(value: any, ids: string[]): any {
  if (typeof value === "string") return value.replace(PLACEHOLDER, (_m, n) => ids[Number(n)] ?? _m);
  if (Array.isArray(value)) return value.map((item) => replaceIds(item, ids));
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [replaceIds(k, ids), replaceIds(v, ids)]));
  return value;
}

function same(a: any, b: any): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) return a.length === b.length && a.every((item, i) => same(item, b[i]));
  for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
    const left = a[key], right = b[key];
    if (left === right) continue;
    if (left === undefined && (right === false || (Array.isArray(right) && right.length === 0))) continue;
    if (right === undefined && (left === false || (Array.isArray(left) && left.length === 0))) continue;
    if (!same(left, right)) return false;
  }
  return true;
}

/** Gives back the caller's own objects wherever Rust's answer equals them, and the blank fields Rust skips. */
function share(before: any, after: any): any {
  if (same(before, after)) return before;
  if (Array.isArray(after)) {
    if (!Array.isArray(before)) return after;
    const byId = new Map(before.filter((item: any) => item && typeof item === "object" && "id" in item).map((item: any) => [item.id, item]));
    return after.map((item: any, i: number) => share(item && typeof item === "object" && "id" in item && byId.has(item.id) ? byId.get(item.id) : before[i], item));
  }
  if (after && typeof after === "object" && before && typeof before === "object" && !Array.isArray(before)) {
    const out: any = {};
    for (const key of Object.keys(after)) out[key] = share(before[key], after[key]);
    for (const key of Object.keys(before)) {
      if (key in after) continue;
      const was = before[key];
      if (typeof was === "boolean" && /^[a-z][a-zA-Z]*$/.test(key)) out[key] = false;
    }
    return out;
  }
  return after;
}

export function rustReduce(state: any, input: any): any {
  const answer = ask({ now: Date.now(), state, input });
  if (answer.error) throw new Error(`rust reduce: ${answer.error}`);
  const count = Number(answer.ids ?? 0);
  const ids = Array.from({ length: count }, () => crypto.randomUUID());
  const fixed = count ? replaceIds(answer, ids) : answer;
  const next = share(state, fixed.state);
  return { state: next, effects: fixed.effects ?? [], ...(fixed.result !== undefined && fixed.result !== null ? { result: fixed.result } : {}) };
}

function diff(a: any, b: any, at = "$", out: string[] = []): string[] {
  if (out.length > 20) return out;
  if (a === b) return out;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") {
    if (typeof a === "number" && typeof b === "number" && Math.abs(a - b) < 1e-9) return out;
    if (typeof a === "number" && typeof b === "number" && a > 1e12 && Math.abs(a - b) < 200) return out;
    out.push(`${at}: ts=${JSON.stringify(a)?.slice(0, 300)} rust=${JSON.stringify(b)?.slice(0, 300)}`);
    return out;
  }
  if (Array.isArray(a) !== Array.isArray(b)) { out.push(`${at}: array mismatch ts=${JSON.stringify(a).slice(0, 300)} rust=${JSON.stringify(b).slice(0, 300)}`); return out; }
  if (Array.isArray(a)) {
    if (a.length !== b.length) out.push(`${at}: length ts=${a.length} rust=${b.length} ts=${JSON.stringify(a).slice(0, 400)} rust=${JSON.stringify(b).slice(0, 400)}`);
    for (let i = 0; i < Math.min(a.length, b.length); i++) diff(a[i], b[i], `${at}[${i}]`, out);
    return out;
  }
  for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
    const left = a[key], right = b[key];
    if (left === undefined && right === undefined) continue;
    const blank = (value: any) => value === undefined || value === null || value === false;
    if (blank(left) && blank(right)) continue;
    if ((left === undefined) !== (right === undefined)) { out.push(`${at}.${key}: ts=${JSON.stringify(left)?.slice(0, 300)} rust=${JSON.stringify(right)?.slice(0, 300)}`); continue; }
    diff(left, right, `${at}.${key}`, out);
  }
  return out;
}

let depth = 0;
const log = process.env.ACT_RUST_LOG!;

/** Runs the TS reducer, runs Rust on the same input, logs where they differ, and answers with TS's transition. */
export function differential(tsReduce: (state: any, input: any) => any, state: any, input: any): any {
  if (depth > 0) return tsReduce(state, input);
  const clock = Date.now;
  const read: number[] = [];
  const original = globalThis.crypto.randomUUID.bind(globalThis.crypto);
  const ids: string[] = [];
  const recording = () => { const id = original(); ids.push(id); return id; };
  let ts: any;
  depth++;
  const descriptor = Object.getOwnPropertyDescriptor(globalThis.crypto, "randomUUID");
  try {
    Object.defineProperty(globalThis.crypto, "randomUUID", { value: recording, configurable: true, writable: true });
    Date.now = () => { const at = clock.call(Date); read.push(at); return at; };
    ts = tsReduce(state, input);
  } finally {
    Date.now = clock;
    if (descriptor) Object.defineProperty(globalThis.crypto, "randomUUID", descriptor); else delete (globalThis.crypto as any).randomUUID;
    depth--;
  }
  let problems: string[];
  try {
    const answer = ask({ now: read[0] ?? clock.call(Date), state, input });
    if (answer.error) problems = [`error: ${answer.error.slice(0, 600)}`];
    else {
      const fixed = replaceIds(answer, ids);
      const left = JSON.parse(JSON.stringify({ state: ts.state, effects: ts.effects, result: ts.result ?? null }, plain));
      for (const effect of left.effects ?? []) if (effect.type === "start-run" && effect.command?.type === "start") delete effect.command.type;
      problems = [...diff(left.state, fixed.state, "$state"), ...diff(left.effects, fixed.effects ?? [], "$effects"), ...diff(left.result, fixed.result ?? null, "$result")];
      if (Number(answer.ids ?? 0) !== ids.length) problems.push(`ids: ts=${ids.length} rust=${answer.ids}`);
    }
  } catch (error) {
    problems = [`threw: ${error}`];
  }
  if (problems.length) {
    const current = (expect as any).getState?.() ?? {};
    fs.appendFileSync(log, JSON.stringify({ file: String(current.testPath ?? "").split("/tests/")[1], test: current.currentTestName ?? "?", input: JSON.stringify(input, plain).slice(0, 500), problems }) + "\n");
  }
  return ts;
}

const viewBinary = process.env.ACT_VIEW_BIN!;
const hostBinary = process.env.ACT_HOST_BIN!;
const ids = (threads: any) => (threads ?? []).map((thread: any) => thread.id);
const sortedIds = (set: any) => [...(set ?? [])].sort();

function projectView(view: any): any {
  const c = view.coordination ?? {};
  const location = view.location ? (view.location.kind === "worktree" ? { kind: "worktree", worktree: view.location.worktree.id, threads: view.location.threads } : { kind: view.location.kind }) : { kind: "local" };
  return {
    currentThread: view.currentThread?.id ?? null,
    activityThreads: { priority: ids(view.activityThreads?.priority), running: ids(view.activityThreads?.running), threads: ids(view.activityThreads?.threads), snoozed: ids(view.activityThreads?.snoozed) },
    activityProject: view.activityProject ?? null,
    orderedThreads: ids(view.orderedThreads),
    threadSlots: view.threadSlots,
    threads: ids(view.threads),
    archivedThreads: ids(view.archivedThreads),
    unreadCount: view.unreadCount,
    sideChatAttention: sortedIds(view.sideChatAttention),
    worktreeThreadIds: sortedIds(view.worktreeThreadIds),
    runningThreadIds: sortedIds(view.runningThreadIds),
    monitoringThreadIds: sortedIds(view.monitoringThreadIds),
    blockedThreadIds: sortedIds(view.blockedThreadIds),
    stoppableCoordinatorIds: sortedIds(view.stoppableCoordinatorIds),
    status: view.status, runActive: view.runActive, compacting: view.compacting, fastMode: view.fastMode, engineLocked: view.engineLocked,
    policy: view.policy, engine: view.engine, model: view.model ?? null, effort: view.effort,
    canGoBack: view.canGoBack, canGoForward: view.canGoForward,
    waitingOn: view.waitingOn ?? null,
    limitPositions: Object.fromEntries(view.limitPositions ?? new Map()),
    threadTabs: (view.threadTabs ?? []).map((tab: any) => tab.id),
    sideChats: (view.sideChats ?? []).map((chat: any) => chat.id),
    location,
    workspaceId: view.workspaceId ?? null,
    zenAvailable: view.zenAvailable,
    prompt: view.prompt ?? "",
    find: view.find ? { matches: view.find.matches, index: view.find.index, counting: view.find.counting, hit: view.find.hit ?? null } : null,
    coordination: {
      coordinator: c.coordinator ?? false,
      lead: c.lead?.id ?? null,
      remoteLead: c.remoteLead ?? null,
      members: (c.members ?? []).map((m: any) => ({ id: m.thread.id, status: m.status, summary: m.summary ?? null, steps: m.steps, startedAt: m.startedAt, lastHeard: m.lastHeard, host: m.host?.name ?? null })),
      starts: c.starts ?? [],
      approvals: (c.approvals ?? []).map((a: any) => a.thread.id),
      decisions: (c.decisions ?? []).map((d: any) => [d.thread.id, d.decision.id]),
      proposals: (c.proposals ?? []).map((p: any) => ({ id: p.proposal.id, starting: p.starting })),
      waiting: c.waiting ? { decisions: c.waiting.decisions, suggestions: c.waiting.suggestions, requests: c.waiting.requests, blocked: c.waiting.blocked, failed: c.waiting.failed } : null,
      asking: c.asking ?? null,
      usage: c.usage?.total ?? null,
    },
  };
}

let viewDepth = 0;
/** Derives the TS view, asks Rust for the same projection, logs where they differ, and answers with TS's view. */
export function differentialView(tsDerive: (state: any) => any, state: any): any {
  if (viewDepth > 0) return tsDerive(state);
  viewDepth++;
  let view: any;
  try { view = tsDerive(state); } finally { viewDepth--; }
  let problems: string[];
  try {
    const answer = ask({ now: Date.now(), state }, viewBinary);
    if (answer.error) problems = [`error: ${String(answer.error).slice(0, 600)}`];
    else problems = diff(JSON.parse(JSON.stringify(projectView(view), plain)), answer, "$view");
  } catch (error) {
    problems = [`threw: ${error}`];
  }
  if (problems.length) {
    const current = (expect as any).getState?.() ?? {};
    fs.appendFileSync(log, JSON.stringify({ file: String(current.testPath ?? "").split("/tests/")[1], test: current.currentTestName ?? "?", input: "deriveView", problems }) + "\n");
  }
  return view;
}

let callDepth = 0;
/** Runs a TS projection, asks view_lines for the same, logs where they differ, and answers with TS's. */
export function differentialCall(name: string, ts: () => any, request: any, shape: (value: any) => any = (value) => value): any {
  if (callDepth > 0) return ts();
  callDepth++;
  let value: any;
  try { value = ts(); } finally { callDepth--; }
  let problems: string[];
  try {
    const { host, ...asked } = request;
    const answer = ask({ now: Date.now(), ...asked }, host ? hostBinary : viewBinary);
    if (answer && answer.error && Object.keys(answer).length === 1) problems = ["error: " + String(answer.error).slice(0, 600)];
    else problems = diff(JSON.parse(JSON.stringify(shape(value) ?? null, plain)), answer, "$" + name);
  } catch (error) {
    problems = ["threw: " + error];
  }
  if (problems.length) {
    const current = (expect as any).getState?.() ?? {};
    fs.appendFileSync(log, JSON.stringify({ file: String(current.testPath ?? "").split("/tests/")[1], test: current.currentTestName ?? "?", input: request.args ? `${name} ${JSON.stringify(request.args, plain).slice(0, 400)}` : name, problems }) + "\n");
  }
  return value;
}
