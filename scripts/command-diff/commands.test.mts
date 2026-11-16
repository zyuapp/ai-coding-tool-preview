// Writes every command's fields, each swapped for values from a pool, with whether the TypeScript
// window check (isWorkspaceViewInput) takes the result. Run by scripts/command-diff/run.sh.
import { test } from "vitest";
import { writeFileSync } from "node:fs";
import { isWorkspaceViewInput, workspaceCommandDefinitions } from "../../src/contracts/workspace-view-input.ts";
import { taskCommands, sideChatCommands } from "../../src/contracts/task-commands.ts";
import { annotationCommands, pasteCommands, imageCommands, imageViewCommands, fileCommands } from "../../src/contracts/attachment-commands.ts";
import { cliCommands } from "../../src/contracts/cli-commands.ts";
import { computerUseCommands } from "../../src/contracts/computer-use-commands.ts";
import { usageCommands } from "../../src/contracts/usage-commands.ts";
import { projectCommands } from "../../src/contracts/project-commands.ts";
import { pullRequestCommands } from "../../src/contracts/pull-request-commands.ts";
import { runControlCommands, reviewCommands } from "../../src/contracts/run-commands.ts";
import { worktreeCommands } from "../../src/contracts/worktree-commands.ts";
import { automationCommands } from "../../src/contracts/automation-commands.ts";
import { browserCommands, browserImportCommands } from "../../src/contracts/browser-commands.ts";
import { simulatorCommands } from "../../src/contracts/simulator-commands.ts";
import { dictationCommands } from "../../src/contracts/dictation-commands.ts";
import { diffCommands } from "../../src/contracts/diff-commands.ts";
import { externalAppCommands } from "../../src/contracts/app-commands.ts";
import { terminalCommands } from "../../src/contracts/terminal-commands.ts";
import { remoteCommands } from "../../src/contracts/remote-commands.ts";
import { computerCommands } from "../../src/contracts/computer-commands.ts";
import { engineCommands } from "../../src/contracts/engine-commands.ts";
import { viewCommands } from "../../src/contracts/view-commands.ts";

const long = (n: number) => "a".repeat(n);
const GENERIC: unknown[] = [
  undefined, null, "", " ", "x", "task-1", "/tmp/a", "a\u0000b", long(257), long(1001), long(4097), long(20_000),
  0, 1, -1, 2, 1.5, 6, 19, 20, 24, 168, 1e9, -0.5, true, false, [], ["x"], [""], [1], {}, { kind: "x" },
];

/** Values a field of this name can hold, beside the generic pool, so each command has one it takes. */
const NAMED: Record<string, unknown[]> = {
  role: ["coordinator", "member", "designer"],
  engine: ["claude", "codex", "opencode", "gemini"],
  effort: ["low", "high", "max", "ultra", "galaxy"],
  policy: ["confirm", "plan", "allow-edits", "autonomous", "bypass", "yolo"],
  model: ["opus", "gpt-6.1-sol", "openai/gpt-9", "constructor", "a b"],
  list: ["threads", "destinations", "other"],
  mode: ["uncommitted", "branch", "light", "dark", "system", "other"],
  step: ["targets", "base", "commit", "custom", "merge", "fix-ci", "resolve-conflicts", "create-pull-request", "other"],
  surface: ["any", "browser", "desktop", "other"],
  permission: ["accessibility", "screenRecording", "other"],
  key: ["ArrowUp", "ArrowDown", "Tab", "Enter", "Escape", "Space"],
  hours: [1, 4, 24, 72, 168, 2, 0, "1"],
  delta: [1, -1, 2, 0],
  index: [0, 3, 19, 20, -1, 1.5],
  cols: [1, 80, 0, 1000, 10000, 1.5],
  rows: [1, 24, 0, 1000, 10000, 1.5],
  commit: ["4b87279", "4b87279deadbeef4b87279deadbeef4b87279d", "abc", "zzzzzzz", "4B87279"],
  host: ["mac.tail.ts.net", "127.0.0.1:8787", "https://mac.tail.ts.net", "host:70000", "Mac.local"],
  root: ["/srv/app", "  ", "~/code"],
  computerId: ["studio", ""],
  section: ["general", "appearance", "usage", "phone", "archive", "computer-use", "threads", "priority", "snoozed", "running", "other", "devices", "engines"],
  group: ["sidebar", "working", "failed", "completed", "running", "finished", "done", "decisions", "members", "nowhere", "other"],
  theme: ["dark"],
  sourceId: ["chrome", "safari", "firefox", "arc", "chrome:Default", "other"],
  site: ["github.com", ""],
  source: ["attachment://file/a.png", "message-image://file/x.png?y=1", "attachment://other", "data:image/png;base64,AAAA", "file:///tmp/a.png", "https://x/y.png", "/tmp/a.png", "attachments/a.png", "javascript:alert(1)"],
  gallery: [["attachment://file/a.png"], ["data:image/png;base64,AAAA"], ["javascript:x"], Array.from({ length: 501 }, () => "attachment://file/a.png")],
  range: [{ kind: "branches", base: "main", compare: null }, { kind: "branches", base: "main", compare: "dev" }, { kind: "branches", base: "", compare: null }, { kind: "branches", base: "main" }, { kind: "uncommitted" }, { kind: "branch" }, { kind: "commit", commit: "4b87279" }, { kind: "since", commit: "4b87279" }, { kind: "commit", commit: "zz" }, "branch", "uncommitted"],
  target: [
    { projectId: "p", index: 0 }, { projectId: null, index: 1.5 }, { projectId: "p" },
    { type: "uncommittedChanges" }, { type: "baseBranch", branch: "main" }, { type: "commit", sha: "4b8", title: null }, { type: "commit", sha: "4b8" }, { type: "commit", sha: "4b8", title: "T" }, { type: "custom", instructions: "x" }, { type: "custom" },
    { kind: "uncommitted" }, { kind: "base", branch: "main" }, { kind: "commit", sha: "4b87279" }, { kind: "commit", commit: "4b87279", title: "t" }, { kind: "custom", instructions: "look" },
    { kind: "thread", taskId: null }, { kind: "thread", taskId: "t" }, { kind: "browser", tabId: "b" }, { kind: "terminal", terminalId: "t" }, { kind: "review", owner: "o" }, { kind: "panel", owner: "o", panel: "p" }, { kind: "panel", owner: "o" },
  ],
  destination: [{ kind: "local" }, { kind: "new" }, { kind: "worktree", id: "w" }, { kind: "worktree" }, { kind: "worktree", id: "" }],
  place: [{ project: "p", worktree: true }, { project: "p" }, { project: 1, worktree: false }],
  proposalIds: [["p1"], ["p1", "p2"], [], Array.from({ length: 30 }, (_, i) => `p${i}`)],
  point: [{ anchor: "m1", depth: 0.5 }, null, { anchor: "m1" }],
  sender: [{ threadId: "t", title: "T", computer: "c" }, { threadId: "", title: "T", computer: "c" }, { threadId: "t", computer: "c" }],
  lead: [{ threadId: "t", title: "T", computer: "c" }, { threadId: "t", title: "T", computer: "c", proposalId: "p" }, { threadId: "t", title: "T", computer: "c", proposalId: null }, { threadId: "t", title: "T", computer: "c", proposalId: "" }],
  brief: [{ intent: "do", doneWhen: "done", delivers: "report" }, { intent: "do", doneWhen: "done", delivers: "release" }, { intent: "", doneWhen: "done", delivers: "commit" }, { intent: "do" }],
  attachments: [
    [{ path: "/tmp/a.png", labels: ["a"] }], [{ path: "/tmp/a.png", labels: ["a"], context: null }], [{ path: "/tmp/a.png" }],
    [{ id: "i", source: "data:image/png;base64,AAAA", annotations: [] }], [{ id: "i", source: "data:image/png;base64,AAAA", annotations: [{ kind: "box", x: 0.5, y: 1, width: 2, height: 3, text: "t" }] }],
    [{ id: "i", source: "data:image/png;base64,AAAA", annotations: [{ kind: "circle", x: 0, y: 1, width: 2, height: 3, text: "t" }] }],
    [{ id: "i", source: "data:image/png;base64,AAAA", annotations: [], path: null }], [{ id: "i", source: "data:image/png;base64,AAAA", annotations: [], path: "/tmp/a" }],
  ],
  annotations: [
    [{ id: "a", quote: "q", note: "n" }], [{ id: "a", quote: "q", note: "n", anchor: { kind: "message", messageId: "m", start: 0, end: 1.5 } }],
    [{ id: "a", quote: "q", note: "n", anchor: { kind: "diff", comparison: "c", path: "p", start: "1", end: "2", side: "old" } }],
    [{ id: "a", quote: "q", note: "n", anchor: { kind: "diff", comparison: "c", path: "p", start: 1, end: 2, side: "old" } }], [{ id: "a", quote: "q" }], [{ id: "a", quote: "q", note: "n", anchor: null }],
  ],
  anchor: [{ kind: "message", messageId: "m", start: 0, end: 4 }, { kind: "diff", comparison: "c", path: "p", start: "1", end: "2", side: "new" }, { kind: "diff", comparison: "c", path: "p", start: "1", end: "2", side: "both" }, { kind: "message", messageId: "m", start: "0", end: 4 }],
  pastes: [[{ id: "p", text: "t" }], [{ id: "p" }], [{ id: "p", text: null }]],
  files: [[{ path: "/a", name: "a" }], [{ path: "/a", name: "a", folder: true }], [{ path: "/a", name: "a", folder: false }], [{ id: "f", path: "/a", name: "a" }], [{ id: "f", path: "/a", name: "a", folder: true }], [{ id: "f", path: "/a", name: "a", folder: null }]],
  paths: [["/a"], [1]],
  roots: [["/a"], []],
  draft: [
    { schedule: "0 8 * * *", prompt: "Check", policy: "confirm" }, { schedule: "0 8 * * *", prompt: "Check" }, { schedule: "0 8 * * *", prompt: "Check", surfaceWhen: "x", endsWhen: "y", timezone: "UTC" },
    { schedule: "", prompt: "Check" }, { schedule: "nope", prompt: "Check" }, { schedule: "0 8 * * *", prompt: "" }, { schedule: "2026-12-01T10:00:00Z", prompt: "Once" },
  ],
  patch: [{ paused: true }, { schedule: "0 9 * * *" }, { prompt: "p" }, { paused: "yes" }, {}, { policy: "bypass" }, { surfaceWhen: "" }, { endsWhen: "x" }, { timezone: "UTC" }],
  action: [
    { kind: "click", ref: "e1" }, { kind: "type", ref: "e1", text: "hi" }, { kind: "type", ref: "e1", text: "hi", submit: true }, { kind: "scroll", dy: 100 }, { kind: "back" },
    { kind: "click" }, { kind: "press", key: "Enter" }, { kind: "wait", condition: "text", value: "x" }, { kind: "wait", condition: "network-idle" }, { kind: "eval", code: "1" },
  ],
  input: [
    { kind: "tap", x: 10, y: 20 }, { kind: "swipe", x: 0, y: 0, toX: 10, toY: 10 }, { kind: "type", text: "hi" }, { kind: "button", button: "home" }, { kind: "button", button: "lock" },
    { kind: "key", key: "a" }, { kind: "paste", text: "x" }, { kind: "copy" }, { kind: "tap", x: "1", y: 2 }, { kind: "rotate", orientation: "portrait" }, { kind: "drag", points: [] },
  ],
  options: [{ sound: true, focus: false }, { sound: true }, { sound: true, focus: 1 }, { hideCursor: true }, { sound: false, hideCursor: false }, {}, { sound: "x" }],
  base: [{ schedule: "s", prompt: "p", surfaceWhen: "", endsWhen: "" }, { schedule: "s", prompt: "p" }],
  fields: [{ schedule: "s", prompt: "p", surfaceWhen: "", endsWhen: "" }, { schedule: "s" }],
  title: ["Title", "  ", long(121), long(120)],
  then: ["go on", "  ", long(100_001)],
  memory: ["remember", long(100_001)],
  text: ["hello", long(257)],
  data: ["ls\r", long(100_001)],
  id: [1, 2.5, "1"],
  size: [14, 0, -1, 13.5],
  name: ["Mac", null],
  warning: ["w"],
  quiet: [true, false],
  afterRun: [true, false],
  ifWaiting: [true, false],
  start: [true, false, 0, "1"],
  folder: [true, false],
  line: [1, 1.5, -1],
};

const TABLES = [taskCommands, sideChatCommands, annotationCommands, pasteCommands, imageCommands, imageViewCommands, fileCommands, cliCommands, computerUseCommands, usageCommands, projectCommands, pullRequestCommands, runControlCommands, reviewCommands, worktreeCommands, automationCommands, browserCommands, browserImportCommands, simulatorCommands, dictationCommands, diffCommands, externalAppCommands, terminalCommands, remoteCommands, computerCommands, engineCommands, viewCommands];
const DECLARED: Record<string, { fields: Record<string, (value: unknown) => boolean> }> = Object.assign({}, ...TABLES);

test("command pool", () => {
  const lines: string[] = [];
  const missing: string[] = [];
  for (const { type, fields } of workspaceCommandDefinitions()) {
    const pool = (field: string) => [...(NAMED[field] ?? []), ...GENERIC];
    const checks = DECLARED[type]!.fields;
    const base: Record<string, unknown> = { type };
    let complete = true;
    for (const field of fields) {
      const takes = pool(field).filter((value) => checks[field]!(value));
      if (!takes.length) { complete = false; missing.push(`${type}.${field}`); continue; }
      if (!takes.includes(undefined)) base[field] = takes[0];
    }
    if (!complete) continue;
    const valid = isWorkspaceViewInput(base);
    if (!valid) missing.push(`${type} (base refused: ${JSON.stringify(base)})`);
    lines.push(JSON.stringify({ input: base, ts: valid }));
    for (const field of fields) {
      for (const value of pool(field)) {
        const input = { ...base };
        if (value === undefined) delete input[field]; else input[field] = value;
        lines.push(JSON.stringify({ input, ts: isWorkspaceViewInput(input), field }));
      }
    }
  }
  writeFileSync(process.env.ACT_COMMANDS_OUT ?? "/tmp/act-commands.jsonl", lines.join("\n") + "\n");
  console.log(`${lines.length} cases`);
  if (missing.length) console.log("NO SAMPLE:\n" + missing.join("\n"));
});
