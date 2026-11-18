"""A stand-in Claude Code or Codex app-server that answers the handshake and replies to each turn with
seeded, mutated protocol messages. `FAKE_ENGINE` picks the dialect, `FAKE_SEED` the noise. A thread
id starting `lost` is one the engine no longer has; a Claude prompt saying `close input` stops
reading input mid-turn."""
import json, os, random, sys, time

dialect = os.environ.get("FAKE_ENGINE", "claude")
if len(sys.argv) > 1 and sys.argv[1] in ("--version", "-v", "version"):
    print("2.1.300 (Claude Code)" if dialect == "claude" else "codex-cli 0.200.0")
    sys.exit(0)
rng = random.Random(int(os.environ.get("FAKE_SEED", "1")))
NOISE = [None, True, False, 0, -1, 1.5, -1e308, 1e308, 2**63, "", "é😀\u0000‮", "x" * 4000, [], [None], {}, {"type": "text"}]


def emit(value):
    sys.stdout.write(value if isinstance(value, str) else json.dumps(value))
    sys.stdout.write("\n")
    sys.stdout.flush()


def mutate(value, depth=0):
    if isinstance(value, dict) and value and depth < 6:
        value = dict(value)
        key = rng.choice(list(value))
        roll = rng.random()
        if roll < 0.25:
            value[key] = rng.choice(NOISE)
        elif roll < 0.35:
            del value[key]
        else:
            value[key] = mutate(value[key], depth + 1)
        return value
    if isinstance(value, list) and value and depth < 6:
        value = list(value)
        index = rng.randrange(len(value))
        value[index] = mutate(value[index], depth + 1) if rng.random() < 0.7 else rng.choice(NOISE)
        return value
    return rng.choice(NOISE) if rng.random() < 0.5 else value


def garbage():
    return rng.choice(["{", "not json", "[1,2", '{"type":', "ÿþ", "{}" * 3, "null", "[]", '"text"', "1e999"])


def noisy(seeds, count):
    for _ in range(count):
        roll = rng.random()
        if roll < 0.05:
            emit(garbage())
        elif roll < 0.3:
            emit(rng.choice(seeds))
        else:
            emit(mutate(rng.choice(seeds)))


def claude_seeds():
    session = "fake-session"
    usage = {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 1}
    tool = {"type": "tool_use", "id": "tu1", "name": "Bash", "input": {"command": "ls", "description": "List"}}
    task = {"type": "tool_use", "id": "tu2", "name": "Task", "input": {"description": "Explore", "prompt": "look", "subagent_type": "Explore"}}
    return [
        {"type": "system", "subtype": "init", "session_id": session, "model": "claude-haiku-5", "tools": ["Bash"], "mcp_servers": [], "slash_commands": ["compact"]},
        {"type": "stream_event", "uuid": "u1", "session_id": session, "parent_tool_use_id": None, "event": {"type": "message_start", "message": {"id": "msg1", "model": "claude-haiku-5", "usage": usage}}},
        {"type": "stream_event", "uuid": "u2", "session_id": session, "parent_tool_use_id": None, "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}},
        {"type": "stream_event", "uuid": "u3", "session_id": session, "parent_tool_use_id": None, "event": {"type": "message_delta", "usage": usage, "delta": {"stop_reason": "end_turn"}}},
        {"type": "assistant", "uuid": "u4", "session_id": session, "parent_tool_use_id": None, "message": {"id": "msg1", "model": "claude-haiku-5", "content": [{"type": "text", "text": "Hello"}, tool], "usage": usage}},
        {"type": "assistant", "uuid": "u5", "session_id": session, "parent_tool_use_id": None, "message": {"id": "msg2", "model": "claude-haiku-5", "content": [task], "usage": usage}},
        {"type": "assistant", "uuid": "u6", "session_id": session, "parent_tool_use_id": "tu2", "message": {"id": "msg3", "content": [{"type": "text", "text": "sub"}, tool], "usage": usage}},
        {"type": "user", "uuid": "u7", "session_id": session, "parent_tool_use_id": None, "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu1", "content": "a\nb", "is_error": False}]}},
        {"type": "system", "subtype": "task_started", "session_id": session, "task_id": "k1", "tool_use_id": "tu2", "description": "Explore", "task_type": "local_agent", "prompt": "look"},
        {"type": "system", "subtype": "task_progress", "session_id": session, "task_id": "k1", "tool_use_id": "tu2", "description": "Explore", "last_tool_name": "Read", "usage": {"total_tokens": 100, "tool_uses": 2, "duration_ms": 10}},
        {"type": "system", "subtype": "task_notification", "session_id": session, "task_id": "k1", "tool_use_id": "tu2", "status": "completed", "summary": "done", "output_file": "/tmp/x"},
        {"type": "system", "subtype": "task_started", "session_id": session, "task_id": "w1", "task_type": "local_workflow", "workflow_name": "wf", "description": "wf"},
        {"type": "system", "subtype": "workflow_progress", "session_id": session, "task_id": "w1", "workflow_progress": [{"type": "phase", "index": 0, "title": "One", "status": "running"}]},
        {"type": "system", "subtype": "compact_boundary", "session_id": session, "compact_metadata": {"trigger": "manual", "pre_tokens": 1000, "post_tokens": 100}},
        {"type": "system", "subtype": "status", "session_id": session, "status": "compacting"},
        {"type": "system", "subtype": "api_retry", "session_id": session, "attempt": 1, "max_retries": 10, "retry_delay_ms": 5, "error": "overloaded", "error_status": 529},
        {"type": "system", "subtype": "background_tasks_changed", "session_id": session, "tasks": [{"id": "b1", "task_id": "b1", "type": "local_bash", "command": "sleep 9", "description": "sleep", "status": "running"}]},
        {"type": "system", "subtype": "session_state_changed", "session_id": session, "state": "idle"},
        {"type": "system", "subtype": "commands_changed", "session_id": session, "commands": [{"name": "x", "description": "y"}]},
        {"type": "rate_limit_event", "session_id": session, "rate_limit_info": {"status": "allowed_warning", "resetsAt": 1900000000, "rateLimitType": "five_hour", "utilization": 0.9}},
        {"type": "control_request", "request_id": "c1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -rf /tmp/nothing"}, "tool_use_id": "tu1", "permission_suggestions": []}},
        {"type": "control_request", "request_id": "c2", "request": {"subtype": "mcp_message", "server_name": "aicodingtool-threads", "message": {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}}},
        {"type": "control_request", "request_id": "c3", "request": {"subtype": "hook_callback", "callback_id": "h1", "input": {"hook_event_name": "PreToolUse"}}},
        {"type": "control_cancel_request", "request_id": "c1"},
        {"type": "keep_alive"},
    ]


def claude_result():
    return {"type": "result", "subtype": "success", "is_error": False, "result": "done", "session_id": "fake-session", "num_turns": 1, "duration_ms": 5, "total_cost_usd": 0.0, "usage": {"input_tokens": 1, "output_tokens": 1}, "modelUsage": {}}


def claude_resumes():
    for arg in sys.argv[1:]:
        if arg.startswith("--resume="):
            return arg.split("=", 1)[1]
    return None


def claude_lost(session):
    reason = f"No conversation found with session ID: {session}"
    sys.stderr.write(reason + "\n")
    emit({"type": "result", "subtype": "error_during_execution", "is_error": True, "num_turns": 0, "session_id": session, "errors": [reason]})
    sys.exit(1)


def claude_closes_input():
    session = "fake-session"
    emit({"type": "system", "subtype": "init", "session_id": session, "model": "claude-haiku-5", "tools": [], "mcp_servers": [], "slash_commands": []})
    emit({"type": "stream_event", "uuid": "c1", "session_id": session, "parent_tool_use_id": None, "event": {"type": "message_start", "message": {"id": "msg-closing", "model": "claude-haiku-5"}}})
    emit({"type": "stream_event", "uuid": "c2", "session_id": session, "parent_tool_use_id": None, "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Working"}}})
    os.close(0)
    time.sleep(15)
    sys.exit(0)


def claude():
    seeds = claude_seeds()
    resumes = claude_resumes()
    if resumes and resumes.startswith("lost"):
        claude_lost(resumes)
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        kind = message.get("type")
        if kind == "control_request":
            subtype = message.get("request", {}).get("subtype")
            response = {"commands": [], "models": [], "account": {}, "output_style": "default"} if subtype == "initialize" else {}
            emit({"type": "control_response", "response": {"subtype": "success", "request_id": message.get("request_id"), "response": response}})
            if subtype == "interrupt":
                emit(claude_result())
        elif kind == "user":
            if "close input" in str(message.get("message", {}).get("content")):
                claude_closes_input()
            noisy(seeds, 80)
            if rng.random() < 0.3:
                emit(mutate(claude_result()))
            emit(claude_result())


def codex_notifications(thread, turn):
    item = {"type": "agentMessage", "id": "i1", "text": "Hello"}
    command = {"type": "commandExecution", "id": "i2", "command": "ls", "cwd": "/tmp", "status": "completed", "aggregatedOutput": "a", "exitCode": 0}
    change = {"type": "fileChange", "id": "i3", "status": "completed", "changes": [{"path": "/tmp/a.rs", "kind": {"type": "update"}, "diff": "+a"}]}
    spawn = {"type": "collabAgentToolCall", "id": "i4", "tool": "spawnAgent", "status": "completed", "senderThreadId": thread, "receiverThreadIds": ["child1"], "prompt": "go"}
    base = {"threadId": thread, "turnId": turn}
    notes = [
        ("turn/started", {"threadId": thread, "turn": {"id": turn, "status": "inProgress", "items": []}}),
        ("item/started", {**base, "item": item}),
        ("item/agentMessage/delta", {**base, "itemId": "i1", "delta": "Hel"}),
        ("item/completed", {**base, "item": item}),
        ("item/started", {**base, "item": command}),
        ("item/commandExecution/outputDelta", {**base, "itemId": "i2", "delta": "out"}),
        ("item/completed", {**base, "item": command}),
        ("item/completed", {**base, "item": change}),
        ("item/completed", {**base, "item": spawn}),
        ("item/completed", {"threadId": "child1", "turnId": "ct", "item": item}),
        ("item/reasoning/summaryTextDelta", {**base, "itemId": "r1", "delta": "think", "summaryIndex": 0}),
        ("thread/tokenUsage/updated", {**base, "tokenUsage": {"total": {"totalTokens": 100, "inputTokens": 50}, "last": {"totalTokens": 10}, "modelContextWindow": 200000}}),
        ("turn/plan/updated", {**base, "plan": [{"step": "one", "status": "inProgress"}], "explanation": "x"}),
        ("turn/diff/updated", {**base, "diff": "+a"}),
        ("thread/status/changed", {"threadId": "child1", "status": {"type": "active", "activeFlags": []}}),
        ("thread/started", {"thread": {"id": "child1", "agentNickname": "Kid", "agentRole": "worker", "parentThreadId": thread}}),
        ("account/rateLimits/updated", {"rateLimits": {"primary": {"usedPercent": 50, "windowDurationMins": 300, "resetsAt": 1900000000}}}),
        ("error", {**base, "error": {"message": "boom", "codexErrorInfo": "serverOverloaded"}, "willRetry": True}),
        ("thread/compacted", base),
        ("thread/name/updated", {"threadId": thread, "threadName": "n"}),
    ]
    requests = [
        ("item/commandExecution/requestApproval", {**base, "itemId": "i2", "command": "rm x", "cwd": "/tmp", "reason": "why"}),
        ("item/fileChange/requestApproval", {**base, "itemId": "i3", "reason": "why"}),
        ("item/tool/requestUserInput", {**base, "itemId": "q1", "questions": [{"id": "q", "header": "H", "question": "Pick?", "options": [{"label": "a", "description": "b"}]}]}),
        ("mcpServer/elicitation/request", {**base, "serverName": "s", "message": "Allow the server to run tool \"x\"?", "requestedSchema": {}}),
    ]
    return notes, requests


def codex():
    thread, turn, next_id = "th-fake", "tu-fake", [1000]
    notes, requests = codex_notifications(thread, turn)

    def note(method, params):
        emit({"method": method, "params": params})

    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        method, ident = message.get("method"), message.get("id")
        if ident is None or method is None:
            continue
        params = message.get("params") or {}
        if method in ("thread/resume", "thread/unarchive") and str(params.get("threadId", "")).startswith("lost"):
            emit({"id": ident, "error": {"code": -32600, "message": f"no rollout found for thread id {params['threadId']}"}})
        elif method == "initialize":
            emit({"id": ident, "result": {"userAgent": "codex/0.200.0", "codexHome": "/tmp"}})
        elif method in ("thread/start", "thread/resume", "thread/fork"):
            emit({"id": ident, "result": {"thread": {"id": thread, "preview": "", "path": "/tmp/x.jsonl", "cwd": "/tmp"}, "model": "gpt-6-luna", "reasoningEffort": "low"}})
        elif method == "turn/start":
            emit({"id": ident, "result": {"turn": {"id": turn, "status": "inProgress", "items": []}}})
            for _ in range(80):
                roll = rng.random()
                if roll < 0.05:
                    emit(garbage())
                elif roll < 0.15:
                    method_name, params = rng.choice(requests)
                    next_id[0] += 1
                    emit({"id": next_id[0], "method": method_name, "params": mutate(params) if rng.random() < 0.6 else params})
                else:
                    method_name, params = rng.choice(notes)
                    note(method_name, mutate(params) if rng.random() < 0.7 else params)
            completed = {"threadId": thread, "turn": {"id": turn, "status": "completed", "items": []}}
            if rng.random() < 0.3:
                note("turn/completed", mutate(completed))
            note("turn/completed", completed)
        elif method == "account/read":
            emit({"id": ident, "result": {"account": {"type": "chatgpt", "email": "fake@example.com", "planType": "plus"}, "requiresOpenaiAuth": True}})
        elif method == "turn/interrupt":
            emit({"id": ident, "result": {}})
            note("turn/completed", {"threadId": thread, "turn": {"id": turn, "status": "interrupted", "items": []}})
        else:
            emit({"id": ident, "result": mutate({"data": [], "items": []}) if rng.random() < 0.3 else {"data": []}})


claude() if dialect == "claude" else codex()
