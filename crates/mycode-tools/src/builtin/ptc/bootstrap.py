"""Static bootstrap for a warm `run_code` process.

Frames are a 4-byte big-endian length plus UTF-8 JSON on a private control
channel: an inherited socket descriptor (Unix) or a token-authenticated
loopback TCP socket. Windows does not use a named pipe here: the pipe handle
that Python opens synchronously serializes pending reads and writes, so the
blocked reader thread would deadlock every later frame write. TCP sockets
carry the reader thread and the writer concurrently. stdout and stderr stay
free; `print` is forwarded as a log frame. User code is the body of an async
function and keeps its own line numbers.

The process stays up after a program finishes and waits for the next boot.
Each boot rebuilds the user namespace and restores modules, builtins,
`sys.path`, and the environment captured before any user code. A read-only
(scout) process is started with `-I` and installs an audit hook once, before
user code. The hook is best-effort: it is not a security boundary.
"""

import ast
import asyncio
import builtins
import io
import json
import os
import queue
import select
import socket
import struct
import sys
import threading
import time
import traceback

_MAX_FRAME = 8 * 1024 * 1024
_hold = None
_fd = None
_sock = None
_write_lock = threading.Lock()
_USER_LINES = []
_loop = None
_pending = {}
_next_id = 1
_id_lock = threading.Lock()
_warned = set()
_tools = set()
_inbox = queue.Queue()
_BASE_MODULES = {}
_BASE_ENV = {}
_BASE_PATH = []
_BASE_BUILTINS = {}
_BASE_OS = {}
_BASE_MAIN = {}
_BASE_THREADS = 1
_CHDIR = os.chdir
# Replaced by user code, these make the next boot untrustworthy.
_CRITICAL = ("_write_frame", "_read_frame", "_reader", "_inbox", "_loop", "_rpc", "_install_hook")

_PROCESS = {
    "os.system",
    "os.popen",
    "os.exec",
    "os.spawn",
    "os.posix_spawn",
    "os.fork",
    "os.forkpty",
    "os.kill",
    "os.killpg",
    "os.startfile",
    "subprocess.Popen",
}
_SOCKET = {
    "socket.__new__",
    "socket.bind",
    "socket.connect",
    "socket.getaddrinfo",
    "socket.getnameinfo",
}
_FILE_MUTATION = {
    "os.remove",
    "os.unlink",
    "os.rmdir",
    "os.removedirs",
    "os.mkdir",
    "os.makedirs",
    "os.rename",
    "os.renames",
    "os.replace",
    "os.truncate",
    "os.link",
    "os.symlink",
    "os.chmod",
    "os.lchmod",
    "os.fchmod",
    "os.chown",
    "os.lchown",
    "os.fchown",
    "os.utime",
    "os.chflags",
    "os.lchflags",
    "os.setxattr",
    "os.removexattr",
    "os.mkfifo",
    "os.mknod",
}


def _read_exact(count):
    buf = bytearray()
    while len(buf) < count:
        if _sock is not None:
            chunk = _sock.recv(count - len(buf))
        else:
            chunk = os.read(_fd, count - len(buf))
        if not chunk:
            raise EOFError("control channel closed")
        buf += chunk
    return bytes(buf)


def _send_all(data):
    if _sock is not None:
        _sock.sendall(data)
        return
    view = memoryview(data)
    while view:
        written = os.write(_fd, view)
        if written <= 0:
            raise EOFError("control channel closed")
        view = view[written:]


def _read_frame():
    header = _read_exact(4)
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > _MAX_FRAME:
        raise ValueError("control frame length %s" % length)
    return json.loads(_read_exact(length).decode("utf-8"))


def _write_frame(obj):
    data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    if len(data) > _MAX_FRAME:
        raise ValueError("control frame is too large")
    packet = struct.pack(">I", len(data)) + data
    with _write_lock:
        _send_all(packet)


class ToolCallError(Exception):
    def __init__(self, toolName, message):
        super().__init__(message)
        self.toolName = toolName
        self.message = message


class _Log(io.TextIOBase):
    def write(self, text):
        if text:
            try:
                _write_frame({"type": "log", "text": str(text)})
            except Exception:
                pass
        return len(text) if isinstance(text, str) else 0

    def flush(self):
        return None


def _reader():
    while True:
        try:
            message = _read_frame()
        except Exception:
            # The host closed the control connection. A cell blocked in user
            # code never returns to the inbox, so this process has to die
            # here. Scout's `-I` interpreter uses the same loop. `os._exit`
            # is not a catchable `SystemExit`.
            try:
                _fail_pending("control channel closed")
            except Exception:
                pass
            os._exit(1)
        if not isinstance(message, dict):
            continue
        if message.get("type") == "reply":
            ident = message.get("id")
            with _id_lock:
                future = _pending.pop(ident, None)
            if future is None or _loop is None:
                continue
            try:
                _loop.call_soon_threadsafe(_settle, future, message)
            except RuntimeError:
                _inbox.put(None)
                return
            continue
        _inbox.put(message)


def _settle(future, message):
    if future.done():
        return
    if message.get("ok"):
        future.set_result(message.get("value"))
    else:
        future.set_exception(
            ToolCallError(
                str(message.get("tool") or ""),
                str(message.get("message") or "tool failed"),
            )
        )


def _fail_pending(message):
    with _id_lock:
        items = list(_pending.items())
        _pending.clear()
    if _loop is None:
        return
    for _ident, future in items:
        _loop.call_soon_threadsafe(_reject, future, message)


def _reject(future, message):
    if not future.done():
        future.set_exception(ToolCallError("", message))


async def _rpc(name, payload):
    global _next_id
    if name == "run_code":
        raise ToolCallError(name, "run_code cannot be called from inside a program")
    if name not in _tools:
        known = ", ".join(sorted(_tools))
        raise ToolCallError(name, "unknown tool %s; available: %s" % (name, known))
    future = _loop.create_future()
    with _id_lock:
        ident = _next_id
        _next_id += 1
        _pending[ident] = future
    try:
        _write_frame({"type": "call", "id": ident, "name": name, "args": payload})
    except Exception as exc:
        with _id_lock:
            _pending.pop(ident, None)
        raise ToolCallError(name, str(exc)) from exc
    return await future


def _binder(name):
    async def call(*args, **kwargs):
        if len(args) > 1 or (args and kwargs):
            raise TypeError("pass a dict or keyword arguments, not both")
        if args:
            if not isinstance(args[0], dict):
                raise TypeError("positional tool argument must be a dict")
            payload = args[0]
        else:
            payload = kwargs
        return await _rpc(name, payload)

    return call


class _Tools:
    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        return _binder(name)

    def __getitem__(self, name):
        return _binder(str(name))


def _warn(event):
    if event in _warned:
        return
    _warned.add(event)
    try:
        _write_frame({"type": "warn", "text": event})
    except Exception:
        pass


def _write_open(args):
    mode = ""
    flags = 0
    # `open(path, mode, flags)` puts the mode string in args[1]. `os.open`
    # emits `open(path, None, flags)`, so the write bits can sit in any slot.
    for item in tuple(args)[1:4]:
        if isinstance(item, str) and not mode:
            mode = item
        elif isinstance(item, int):
            flags |= item
    if any(mark in mode for mark in ("w", "a", "x", "+")):
        return True
    write_bits = 0
    # O_TMPFILE is only useful with O_RDWR or O_WRONLY, which are already
    # here. Named so a later edit does not treat that flag as read-only.
    # os.write on an existing fd and os.memfd_create do not emit these events.
    for bit in ("O_WRONLY", "O_RDWR", "O_APPEND", "O_CREAT", "O_TRUNC", "O_TMPFILE"):
        write_bits |= getattr(os, bit, 0)
    return bool(flags & write_bits)


def _install_hook(restricted, warn_process):
    """Best-effort guard. Audit hooks can be bypassed; they are not a sandbox."""

    def hook(event, args):
        process = (
            event in _PROCESS
            or event.startswith("os.exec")
            or event.startswith("os.spawn")
            or event.startswith("os.posix_spawn")
        )
        if process:
            if restricted:
                raise PermissionError("scout python blocked %s" % event)
            if warn_process:
                _warn(event)
            return
        if not restricted:
            return
        if event.startswith("ctypes.") or event.startswith("sqlite3."):
            raise PermissionError("scout python blocked %s" % event)
        if event in _SOCKET:
            raise PermissionError("scout python blocked a network socket")
        if event in _FILE_MUTATION or (event == "open" and _write_open(tuple(args))):
            raise PermissionError(
                "scout python blocked a file write, create, delete, or rename"
            )

    sys.addaudithook(hook)


def _source_line(lineno):
    if not isinstance(lineno, int) or lineno < 1 or lineno > len(_USER_LINES):
        return ""
    return _USER_LINES[lineno - 1].strip()


def _snippet(lineno, fallback):
    text = (fallback or "").strip() or _source_line(lineno)
    return text or "<source unavailable>"


def _format_exc(exc):
    if isinstance(exc, SyntaxError):
        line = exc.lineno or 0
        snippet = _snippet(line, exc.text)
        return "SyntaxError: %s (line %s)\nline %s: %s" % (exc.msg, line, line, snippet)
    rows = []
    for frame in traceback.extract_tb(exc.__traceback__):
        if frame.filename == "<user>":
            snippet = _snippet(frame.lineno, frame.line)
            rows.append("line %s: %s" % (frame.lineno, snippet))
    head = "%s: %s" % (type(exc).__name__, exc)
    if rows:
        return head + "\n" + "\n".join(rows)
    return head


async def _exec_user(code):
    module = ast.parse(code, filename="<user>")
    wrapper = ast.parse("async def __mycode_main__():\n    pass")
    function = wrapper.body[0]
    function.body = module.body or [ast.Pass()]
    ast.fix_missing_locations(function)
    tree = ast.Module(body=[function], type_ignores=[])
    ast.fix_missing_locations(tree)
    compiled = compile(tree, "<user>", "exec", dont_inherit=True)
    namespace = {
        "__name__": "__main__",
        "__builtins__": __builtins__,
        "tools": _Tools(),
        "ToolCallError": ToolCallError,
        "asyncio": asyncio,
    }
    exec(compiled, namespace)  # noqa: S102 - the host sent this program
    return await namespace["__mycode_main__"]()


async def _cancel_stray_tasks():
    current = asyncio.current_task()
    stray = [
        task
        for task in asyncio.all_tasks()
        if task is not current and not task.done()
    ]
    for task in stray:
        task.cancel()
    if stray:
        await asyncio.gather(*stray, return_exceptions=True)


def _can_stay_warm():
    if threading.active_count() > _BASE_THREADS:
        return False
    main = sys.modules[__name__].__dict__
    for key in _CRITICAL:
        baseline = _BASE_MAIN.get(key)
        if baseline is not None and main.get(key) is not baseline:
            return False
    with _id_lock:
        if _pending:
            return False
    return True


async def _finish(payload):
    await _cancel_stray_tasks()
    payload["warm"] = _can_stay_warm()
    with _id_lock:
        _pending.clear()
    _write_frame(payload)


async def _run_user(code):
    try:
        result = await _exec_user(code)
    except Exception as exc:
        await _finish(
            {
                "type": "done",
                "error": {"kind": "exception", "message": _format_exc(exc)},
            }
        )
        return
    try:
        json.dumps(result)
    except TypeError as exc:
        await _finish(
            {
                "type": "done",
                "error": {
                    "kind": "invalid-output",
                    "message": "return value is not JSON: %s" % exc,
                },
            }
        )
        return
    await _finish({"type": "done", "value": result})


def _capture_baseline():
    """Snapshot interpreter state after the hook and before user code."""
    global _BASE_MODULES, _BASE_ENV, _BASE_PATH, _BASE_BUILTINS, _BASE_OS, _BASE_MAIN
    global _BASE_THREADS, _CHDIR
    _BASE_MODULES = dict(sys.modules)
    _BASE_ENV = dict(os.environ)
    _BASE_PATH = list(sys.path)
    _BASE_BUILTINS = dict(builtins.__dict__)
    _BASE_OS = dict(os.__dict__)
    _BASE_THREADS = threading.active_count()
    _CHDIR = os.chdir
    # Taken last so the snapshot sees the containers above. The snapshot's
    # own `_BASE_MAIN` entry still points at the previous dict; restore skips
    # that key so it cannot wipe this snapshot.
    _BASE_MAIN = dict(sys.modules[__name__].__dict__)


def _restore_modules():
    current = sys.modules
    for name in list(current):
        if name not in _BASE_MODULES:
            current.pop(name, None)
    for name, module in _BASE_MODULES.items():
        if current.get(name) is not module:
            current[name] = module


def _restore_main():
    main = sys.modules[__name__].__dict__
    for key in list(main):
        if key not in _BASE_MAIN:
            del main[key]
    for key, value in _BASE_MAIN.items():
        # Assigned for every boot after this restore. `_BASE_MAIN` is this
        # snapshot; writing the stale entry back would drop it.
        if key in ("_tools", "_USER_LINES", "_next_id", "_BASE_MAIN"):
            continue
        if main.get(key) is not value:
            main[key] = value
    _warned.clear()


def _restore_builtins():
    current = builtins.__dict__
    for key in list(current):
        if key not in _BASE_BUILTINS:
            try:
                del current[key]
            except Exception:
                pass
    for key, value in _BASE_BUILTINS.items():
        if current.get(key) is not value:
            try:
                current[key] = value
            except Exception:
                pass


def _restore_os():
    """Undo monkeypatches on the existing `os` module object.

    Replacing the module in `sys.modules` is handled separately. Internal
    `os` state (open fds, the environ mapping object) is not rebuilt here.
    """
    current = os.__dict__
    for key, value in _BASE_OS.items():
        if key.startswith("_"):
            continue
        if current.get(key) is not value:
            try:
                current[key] = value
            except Exception:
                pass
    for key in list(current):
        if key.startswith("_") or key in _BASE_OS:
            continue
        try:
            del current[key]
        except Exception:
            pass


def _restore_for_call():
    _restore_modules()
    _restore_main()
    _restore_builtins()
    _restore_os()
    try:
        os.environ.clear()
        os.environ.update(_BASE_ENV)
    except Exception:
        pass
    try:
        sys.path[:] = list(_BASE_PATH)
    except Exception:
        sys.path = list(_BASE_PATH)
    sys.stdout = _Log()
    sys.stderr = _Log()


def _begin_call(boot):
    """Reset user-visible state, then apply this boot. Isolation is fixed."""
    global _USER_LINES, _tools
    _restore_for_call()
    code = boot.get("code") or ""
    if not isinstance(code, str):
        code = ""
    _USER_LINES = code.splitlines()
    tools = boot.get("tools") or []
    _tools = set(tools) if isinstance(tools, list) else set()
    cwd = boot.get("cwd") or ""
    if isinstance(cwd, str) and cwd:
        _CHDIR(cwd)
    isolated = bool(getattr(sys.flags, "isolated", 0))
    if bool(boot.get("restricted")) != isolated:
        raise SystemExit("python isolation does not match the process")
    return code


def _open_control():
    """Attach the channel the host prepared and return the handshake token.

    Unix delivers the token on the inherited socket, so it is never part of
    the exec environment (`/proc/self/environ` keeps that initial block).
    TCP reads it from a 0600 file and deletes the file before ready is sent.
    """
    global _hold, _fd, _sock
    inherited = os.environ.get("MYCODE_PTC_FD")
    port = os.environ.get("MYCODE_PTC_PORT")
    if inherited:
        _hold = os.fdopen(int(inherited), "r+b", buffering=0)
        _fd = _hold.fileno()
        message = _read_frame()
        if message.get("type") != "token":
            raise SystemExit("expected token")
        return str(message.get("token") or "")
    if port:
        sock = socket.create_connection(("127.0.0.1", int(port)), timeout=30)
        try:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass
        sock.settimeout(None)
        _hold = sock
        _sock = sock
        _fd = sock.fileno()
        return _read_token_file()
    raise SystemExit("no control channel")


def _read_token_file():
    path = os.environ.get("MYCODE_PTC_TOKEN_FILE")
    if not path:
        return ""
    with open(path, "r", encoding="utf-8") as handle:
        token = handle.read().strip()
    try:
        os.remove(path)
    except OSError:
        pass
    return token


def _drop_channel_env():
    for key in (
        "MYCODE_PTC_TOKEN",
        "MYCODE_PTC_FD",
        "MYCODE_PTC_PORT",
        "MYCODE_PTC_TOKEN_FILE",
    ):
        os.environ.pop(key, None)


def _graceful_close():
    """Tear the control channel down before interpreter shutdown.

    The reader thread is a daemon blocked in recv. If process exit closes
    the socket under it, Windows raises an RST that discards frames still
    sitting in the host's receive buffer — the final done frame included.
    Shutdown unblocks the reader, flushes the send side behind a clean FIN,
    and close() then runs with no pending I/O.
    """
    global _hold, _sock, _fd
    if _sock is not None:
        try:
            _sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            _sock.close()
        except OSError:
            pass
    elif _hold is not None:
        try:
            _hold.close()
        except OSError:
            pass
    _hold = None
    _sock = None
    _fd = None


def _watch_parent():
    """Exit when the host process dies.

    macOS has no parent-death signal. Closing the control socket is enough
    only once this thread notices; a blocked cell would otherwise stay up.
    `kqueue` reports the parent process exiting. Polling `getppid` covers a
    kernel that rejects the watch.
    """
    parent = os.getppid()
    if parent <= 1:
        os._exit(1)
    try:
        _watch_parent_kqueue(parent)
    except Exception:
        while True:
            time.sleep(0.2)
            current = os.getppid()
            if current != parent or current <= 1:
                os._exit(1)


def _watch_parent_kqueue(parent):
    kq = select.kqueue()
    event = select.kevent(
        parent,
        filter=select.KQ_FILTER_PROC,
        flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE,
        fflags=select.KQ_NOTE_EXIT,
    )
    kq.control([event], 0, 0)
    while True:
        if kq.control(None, 1, 1.0):
            os._exit(1)
        current = os.getppid()
        if current != parent or current <= 1:
            os._exit(1)


def main():
    global _loop
    token = _open_control()
    _write_frame({"type": "ready", "token": token})
    # Drop the local before user code runs. The listener is already closed
    # once the host accepts this ready frame; keeping the secret in `main`
    # would still let `inspect` read it off this frame.
    del token
    _drop_channel_env()
    sys.stdout = _Log()
    sys.stderr = _Log()
    # The event loop opens a self-pipe. Install the audit hook after that,
    # once, so a later boot cannot skip it and scout still blocks sockets
    # opened after the first program. `-I` is a process flag: a boot cannot
    # turn isolation off, and a non-isolated process cannot claim to be scout.
    _loop = asyncio.new_event_loop()
    asyncio.set_event_loop(_loop)
    threading.Thread(target=_reader, name="mycode-ptc", daemon=True).start()
    # Start before the thread snapshot. A later thread would make every
    # program look like it leaked a thread and refuse to stay warm.
    if sys.platform == "darwin":
        threading.Thread(
            target=_watch_parent, name="mycode-ptc-parent", daemon=True
        ).start()
    isolated = bool(getattr(sys.flags, "isolated", 0))
    _install_hook(isolated, not isolated)
    _capture_baseline()
    while True:
        boot = _inbox.get()
        if not isinstance(boot, dict):
            break
        if boot.get("type") == "shutdown":
            break
        if boot.get("type") != "boot":
            continue
        try:
            code = _begin_call(boot)
        except SystemExit:
            raise
        except Exception as exc:
            _write_frame(
                {
                    "type": "done",
                    "error": {"kind": "exception", "message": _format_exc(exc)},
                    "warm": True,
                }
            )
            continue
        try:
            _loop.run_until_complete(_run_user(code))
        except BaseException:
            _graceful_close()
            raise
        if _loop.is_closed():
            break
    _graceful_close()


if __name__ == "__main__":
    main()
