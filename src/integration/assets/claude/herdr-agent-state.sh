#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=12

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import re
import shutil
import socket
import time

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

if "CURSOR_VERSION" in os.environ or "cursor_version" in hook_input:
    raise SystemExit(0)
hook_event_name = str(hook_input.get("hook_event_name") or "")
if hook_event_name != "SessionStart":
    raise SystemExit(0)
is_subagent = bool(hook_input.get("agent_id"))
if is_subagent:
    raise SystemExit(0)
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
if not agent_session_id:
    raise SystemExit(0)
transcript_path = hook_input.get("transcript_path")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None
session_start_source = hook_input.get("source")
if not isinstance(session_start_source, str) or not session_start_source:
    session_start_source = None

# Launch options a resumed session keeps. Everything else is dropped: prompts,
# session selection (--resume, --continue, --session-id), --model and --effort
# (Claude Code and the user's shell restore those from the transcript), and
# one-shot print options.
KEPT_FLAGS = {
    "--allow-dangerously-skip-permissions",
    "--ax-screen-reader",
    "--bare",
    "--brief",
    "--chrome",
    "--dangerously-skip-permissions",
    "--disable-slash-commands",
    "--exclude-dynamic-system-prompt-sections",
    "--ide",
    "--no-chrome",
    "--restricted",
    "--safe-mode",
    "--strict-mcp-config",
    "--verbose",
}
KEPT_VALUE_OPTIONS = {
    "--agent",
    "--agents",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--autocompact",
    "--debug-file",
    "--fallback-model",
    "--permission-mode",
    "--permission-prompts",
    "--plugin-dir",
    "--setting-sources",
    "--settings",
    "--system-prompt",
    "--system-prompt-file",
    "--system-prompt-snapshot",
}
KEPT_LIST_OPTIONS = {
    "--add-dir",
    "--allowed-tools",
    "--allowedTools",
    "--betas",
    "--disallowed-tools",
    "--disallowedTools",
    "--mcp-config",
    "--tools",
}
DROPPED_VALUE_OPTIONS = {
    "--effort",
    "--environment",
    "--input-format",
    "--json-schema",
    "--max-budget-usd",
    "--model",
    "--name",
    "--output-format",
    "--plugin-url",
    "--remote-control-session-name-prefix",
    "--session-id",
    "-n",
}
DROPPED_OPTIONAL_VALUE_OPTIONS = {
    "--cloud",
    "--debug",
    "--prompt-suggestions",
    "--remote-control",
    "--resume",
    "--teleport",
    "--worktree",
    "-d",
    "-r",
    "-w",
}
NOT_RESUMABLE = {"-p", "--print", "--no-session-persistence"}


def process_argv(pid):
    with open(f"/proc/{pid}/cmdline", "rb") as handle:
        return [arg.decode("utf-8", "surrogateescape") for arg in handle.read().split(b"\0")[:-1]]


def claude_launch_args():
    """Arguments of the Claude process running this hook, or [] when unknown."""
    claude_pid = os.environ.get("CLAUDE_PID", "")
    if claude_pid.isdigit():
        try:
            return process_argv(int(claude_pid))[1:]
        except OSError:
            pass
    pid = os.getppid()
    for _ in range(8):
        if pid <= 1:
            return []
        try:
            argv = process_argv(pid)
            with open(f"/proc/{pid}/comm", encoding="utf-8") as handle:
                comm = handle.read().strip()
        except OSError:
            return []
        if argv and (comm == "claude" or os.path.basename(argv[0]) == "claude"):
            return argv[1:]
        try:
            with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
                pid = int(handle.read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            return []
    return []


def kept_launch_options(args):
    """Options to repeat on resume, or None when the session cannot be resumed."""
    kept = []
    index = 0
    while index < len(args):
        arg = args[index]
        index += 1
        if arg == "--":
            break
        if arg.startswith("--"):
            name, equals, _ = arg.partition("=")
            inline = bool(equals)
        else:
            name, inline = arg, False
        if name in NOT_RESUMABLE:
            return None
        if name in KEPT_FLAGS and not inline:
            kept.append(arg)
        elif name in KEPT_VALUE_OPTIONS:
            if inline:
                kept.append(arg)
            elif index < len(args):
                kept.extend([arg, args[index]])
                index += 1
        elif name in KEPT_LIST_OPTIONS:
            if inline:
                kept.append(arg)
                continue
            values = []
            while index < len(args) and not args[index].startswith("-"):
                values.append(args[index])
                index += 1
            if values:
                kept.extend([arg, *values])
        elif name in DROPPED_VALUE_OPTIONS:
            if not inline and index < len(args):
                index += 1
        elif name in DROPPED_OPTIONAL_VALUE_OPTIONS:
            if not inline and index < len(args) and not args[index].startswith("-"):
                index += 1
    return kept


def resume_command():
    """The command that starts Claude under this session's login.

    `~/.claude-<id>` is the convention for a second login whose `claude-<id>`
    wrapper sets CLAUDE_CONFIG_DIR itself; any other directory is passed
    explicitly through env.
    """
    config_dir = os.environ.get("CLAUDE_CONFIG_DIR", "").strip()
    if not config_dir:
        return ["claude"]
    home = os.path.expanduser("~")
    path = os.path.normpath(os.path.expanduser(config_dir))
    name = os.path.basename(path)
    if os.path.dirname(path) == home:
        if name == ".claude":
            return ["claude"]
        if re.fullmatch(r"\.claude-[A-Za-z0-9_.-]+", name) and shutil.which(name[1:]):
            return [name[1:]]
    return ["env", f"CLAUDE_CONFIG_DIR={path}", "claude"]


def valid_resume_argv(argv):
    # Mirrors Herdr's validate_resume_argv, which rejects the whole report.
    return (
        len(argv) <= 64
        and sum(len(arg) for arg in argv) <= 8192
        and not any("'" in arg or any(ord(ch) < 32 or ord(ch) == 127 for ch in arg) for arg in argv)
    )


def build_resume_argv():
    options = kept_launch_options(claude_launch_args())
    if options is None:
        return None
    command = resume_command()
    resume = ["--resume", agent_session_id]
    for argv in (command + options + resume, command + resume):
        if valid_resume_argv(argv):
            return argv
    return None


resume_argv = build_resume_argv()


def build_request():
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": time.time_ns(),
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    if resume_argv:
        params["resume_argv"] = resume_argv
    return {
        "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": "pane.report_agent_session",
        "params": params,
    }


def send(request, timeout=1.0):
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(timeout)
        client.connect(socket_path)
        client.sendall((json.dumps(request) + "\n").encode())
        reply = b""
        try:
            while not reply.endswith(b"\n"):
                chunk = client.recv(4096)
                if not chunk:
                    break
                reply += chunk
        except Exception:
            pass
        client.close()
        return reply
    except Exception:
        return None


def resume_rejected(reply):
    try:
        return json.loads(reply)["error"]["code"] == "resume_not_accepted"
    except Exception:
        return False


if resume_rejected(send(build_request())) and resume_argv and hasattr(os, "fork"):
    # Herdr keeps a resume command only once it has recognised Claude in the
    # pane, which can lag behind SessionStart. Retry briefly in a detached
    # child so Claude's startup does not wait for it.
    if os.fork() == 0:
        try:
            os.setsid()
            devnull = os.open(os.devnull, os.O_RDWR)
            for fd in (0, 1, 2):
                os.dup2(devnull, fd)
            for _ in range(10):
                time.sleep(0.5)
                if not resume_rejected(send(build_request(), timeout=2.0)):
                    break
        finally:
            os._exit(0)
PY
