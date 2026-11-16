# Routes a copy of the TypeScript app's reducer, deriveView and mobile projection through shim.ts,
# so its test suite runs them beside act_core. Usage: python3 patch.py <ts-copy>
import re
import shutil
import sys
from pathlib import Path

root = Path(sys.argv[1])
app = root / "src/application"
shutil.copy(Path(__file__).with_name("shim.ts"), app / "rust-reduce.ts")


def patch(name, edits, imported):
    path = app / name
    text = path.read_text()
    if "./rust-reduce.js" in text:
        return
    for old, new in edits:
        assert old in text, f"{name}: {old[:60]}"
        text = text.replace(old, new, 1)
    path.write_text(f'import {{ {imported} }} from "./rust-reduce.js";\n' + text)


patch("workspace-reducer.ts", [(
    "export function reduce(state: WorkspaceState, input: WorkspaceInput): WorkspaceTransition {\n",
    "export function reduce(state: WorkspaceState, input: WorkspaceInput): WorkspaceTransition {\n"
    '  if (process.env.ACT_RUST === "diff") return differential(tsReduce, state, input);\n'
    "  if (process.env.ACT_RUST) return rustReduce(state, input);\n"
    "  return tsReduce(state, input);\n}\n\n"
    "function tsReduce(state: WorkspaceState, input: WorkspaceInput): WorkspaceTransition {\n",
)], "differential, rustReduce")

patch("workspace-view.ts", [(
    "export function deriveView(state: WorkspaceState) {\n",
    "export function deriveView(state: WorkspaceState): ReturnType<typeof tsDeriveView> {\n"
    '  if (process.env.ACT_RUST === "diff") return differentialView(tsDeriveView, state);\n'
    "  return tsDeriveView(state);\n}\n\n"
    "function tsDeriveView(state: WorkspaceState) {\n",
)], "differentialView")

mobile = {
    "projectMobileView(state: WorkspaceState, at: number): MobileView": ("projectMobileView", "(state, at)", '{ kind: "mobile", state, at }'),
    "mobileArchivedThreads(state: WorkspaceState): MobileArchivedThread[]": ("mobileArchivedThreads", "(state)", '{ kind: "mobile-archived", state }'),
    "mobileThreadRead(state: WorkspaceState, taskId: string): MobileThreadRead | null": ("mobileThreadRead", "(state, taskId)", '{ kind: "mobile-read", state, taskId }'),
    "mobileThreadHistory(state: WorkspaceState, taskId: string, before: number): MobileThreadHistory | null": ("mobileThreadHistory", "(state, taskId, before)", '{ kind: "mobile-history", state, taskId, before }'),
}
edits = []
for signature, (name, args, request) in mobile.items():
    params = signature[len(name):signature.rindex("):") + 1]
    returns = signature[signature.rindex("):") + 2:]
    edits.append((
        f"export function {signature} {{",
        f"export function {name}{params}:{returns} {{\n"
        f'  if (process.env.ACT_RUST === "diff") return differentialCall("{name}", () => ts_{name}{args}, {request});\n'
        f"  return ts_{name}{args};\n}}\n\nfunction ts_{signature} {{",
    ))
patch("mobile-projection.ts", edits, "differentialCall")


def wrapped(signature, name, args, request, shape="undefined"):
    params = signature[len(name):signature.rindex("):") + 1]
    returns = signature[signature.rindex("):") + 2:]
    call_args = args
    return (
        f"export function {signature} {{",
        f"export function {name}{params}:{returns} {{\n"
        f'  if (process.env.ACT_RUST === "diff") return differentialCall("{name}", () => ts_{name}{call_args}, {request}, {shape});\n'
        f"  return ts_{name}{call_args};\n}}\n\nfunction ts_{signature} {{",
    )


patch("thread-projection.ts", [
    wrapped("resolveScope(state: WorkspaceState, callerThreadId: string, project?: string): ProjectScope | { error: string }", "resolveScope", "(state, callerThreadId, project)",
            '{ kind: "scope", state, caller: callerThreadId, project }', '(value: any) => "error" in value ? { failed: value.error } : value'),
    wrapped("threadWaitResult(state: WorkspaceState, threadId: string, timedOut: boolean): ThreadWaitResult | null", "threadWaitResult", "(state, threadId, timedOut)",
            '{ kind: "wait", state, taskId: threadId, timedOut }'),
    wrapped("threadSummaries(state: WorkspaceState, filter: ThreadFilter, at: number): ThreadSummary[]", "threadSummaries", "(state, filter, at)",
            '{ kind: "summaries", state, filter: { ...filter, stored: filter.stored ? [...filter.stored] : undefined }, at }'),
    wrapped("threadMatches(state: WorkspaceState, reference: string): { rank: number; threads: Thread[] }", "threadMatches", "(state, reference)",
            '{ kind: "matches", state, reference }', "(value: any) => ({ rank: value.rank, threads: value.threads.map((thread: any) => thread.id) })"),
    wrapped("findThread(state: WorkspaceState, reference: string): Thread | null", "findThread", "(state, reference)",
            '{ kind: "find", state, reference }', "(value: any) => value?.id ?? null"),
    (
        "export function threadTranscript(state: WorkspaceState, threadId: string, limit = DEFAULT_TRANSCRIPT_MESSAGES, textLimit = MESSAGE_TEXT_LIMIT, end?: number): ThreadTranscript | null {",
        "export function threadTranscript(state: WorkspaceState, threadId: string, limit = DEFAULT_TRANSCRIPT_MESSAGES, textLimit = MESSAGE_TEXT_LIMIT, end?: number): ThreadTranscript | null {\n"
        '  if (process.env.ACT_RUST === "diff") return differentialCall("threadTranscript", () => ts_threadTranscript(state, threadId, limit, textLimit, end), { kind: "transcript", state, taskId: threadId, limit, textLimit, end });\n'
        "  return ts_threadTranscript(state, threadId, limit, textLimit, end);\n}\n\n"
        "function ts_threadTranscript(state: WorkspaceState, threadId: string, limit = DEFAULT_TRANSCRIPT_MESSAGES, textLimit = MESSAGE_TEXT_LIMIT, end?: number): ThreadTranscript | null {",
    ),
], "differentialCall")

# The model test's approvals predate the tool id every approval intent carries.
model = root / "tests/application/reducer-model.test.mts"
text = model.read_text()
model.write_text(text.replace('intent: { name: "Bash", input: { command: "ls" } }', 'intent: { toolId: "tool-1", name: "Bash", input: { command: "ls" } }'))


# Pure helpers the tests call directly, each checked against view_lines' "fn" counterpart.
IDS = "(value: any) => value.map((item: any) => item.id)"
SECTIONS = "(value: any) => Object.fromEntries(Object.entries(value).map(([key, threads]: [string, any]) => [key, threads.map((thread: any) => thread.id)]))"
SORTED = "(set: any) => [...set].sort()"
helpers = {
    "thread-order.ts": {"orderThreads": IDS, "activitySections": SECTIONS, "recentIn": IDS, "nextSortIndex": None, "threadMoveBlock": None,
                        "backfillSortIndex": "(value: any) => value.map((thread: any) => [thread.id, thread.sortIndex ?? null])",
                        "moveThread": "(value: any) => value.map((thread: any) => [thread.id, thread.sortIndex ?? null, thread.projectId ?? null])"},
    "coordination.ts": {"coordinationSections": SECTIONS},
    "project-order.ts": {"orderProjects": IDS},
    "thread-activity.ts": {"threadStatus": None, "waitingOn": None,
                           "threadLists": f"(value: any) => ({{ listedThreads: value.listedThreads.map((t: any) => t.id), visibleThreads: value.visibleThreads.map((t: any) => t.id), archivedThreads: value.archivedThreads.map((t: any) => t.id), worktreeThreadIds: ({SORTED})(value.worktreeThreadIds), sideChatAttention: ({SORTED})(value.sideChatAttention), unreadCount: value.unreadCount }})",
                           "threadActivity": f"(value: any) => Object.fromEntries(Object.entries(value).map(([key, set]: [string, any]) => [key, ({SORTED})(set)]))"},
    "coordinated-thread-status.ts": {"nextSteps": None, "coordinatedThreadStatus": "(value: any) => ({ status: value.status, summary: value.summary })"},
    "pastes.ts": {"promptWithPastes": None},
    "annotations.ts": {"promptWithAnnotations": None, "clampQuote": None},
    "attachments.ts": {"promptWithAttachments": None, "threadTitleFor": None},
    "files.ts": {"promptWithFiles": None},
    "composer-drafts.ts": {"textAfterSend": None},
}
STATEFUL = {"threadStatus", "waitingOn", "threadLists", "threadActivity"}
domain = {
    "../domain/find.ts": {"findHits": None, "stepMatch": None},
    "../domain/simulator.ts": {"isSimulatorInput": None, "hidUsage": "(value: any) => value ?? null"},
    "../domain/thread-storage.ts": {"parseThreadStore": None, "migrateV1ToV2": None, "serializeThreadStore": "(value: any) => Object.fromEntries(Object.entries(value).map(([key, text]: [string, any]) => [key, typeof text === \"string\" ? JSON.parse(text) : text]))"},
    "../domain/shortcuts.ts": {name: None for name in ["parseShortcut", "formatShortcut", "keystrokeOf", "displayShortcut", "desktopAccelerator", "shortcutProblem", "resolveShortcuts", "withShortcut", "shortcutFor"]},
}
hosted = {
    "workspace-patches.ts": {"workspacePatches": None, "applyWorkspacePatches": None},
    "../contracts/mobile.ts": {"isMobileClientMessage": None, "isMobileQuery": None},
    "../contracts/computers.ts": {"isComputerClientRequest": None, "isComputerQuery": None, "isComputerServerMessage": None},
}
contracts = {
    "../contracts/workspace-view-input.ts": {"isExternalCommand": None, "isPhoneCommand": None},
    "../contracts/computer-capabilities.ts": {"supportsComputerCommand": None},
}
for name, functions in [*helpers.items(), *domain.items(), *contracts.items(), *hosted.items()]:
    path = app / name
    text = path.read_text()
    if "rust-reduce.js" in text:
        continue
    for function, shape in functions.items():
        found = re.search(rf"export function {function}(<[^>]*>)?\(", text)
        assert found, f"{name}: {function}"
        text = text[:found.start()] + f"function ts_{function}{found.group(1) or ''}(" + text[found.end():]
        state = "state: args[0], " if function in STATEFUL else "host: true, " if name in hosted else ""
        text += (f"\nexport function {function}(...args: Parameters<typeof ts_{function}>): ReturnType<typeof ts_{function}> {{\n"
                 f'  if (process.env.ACT_RUST === "diff") return differentialCall("{function}", () => (ts_{function} as any)(...args), {{ kind: "fn", name: "{function}", {state}args }}{", " + shape if shape else ""});\n'
                 f"  return (ts_{function} as any)(...args);\n}}\n")
    shim = "../application/rust-reduce.js" if name.startswith("../") else "./rust-reduce.js"
    path.write_text(f'import {{ differentialCall }} from "{shim}";\n' + text)

remote = app / "remote-workspace.ts"
text = remote.read_text()
if "rust-reduce.js" not in text:
    old = "  return (wire: unknown): WorkspaceState => {"
    assert old in text
    text = text.replace(old, "  const tsRead = (wire: unknown): WorkspaceState => {", 1)
    end = "    views.set(wire, result);\n    return result;\n  };\n}"
    assert end in text
    text = text.replace(end, end[:-1] + '  return (wire: unknown): WorkspaceState => process.env.ACT_RUST === "diff" ? differentialCall("readRemoteWorkspace", () => tsRead(wire), { host: true, name: "readRemoteWorkspace", args: [wire] }) : tsRead(wire);\n}', 1)
    remote.write_text('import { differentialCall } from "./rust-reduce.js";\n' + text)
