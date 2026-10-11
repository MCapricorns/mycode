"""Static bootstrap for one `run_code` process.

The host owns a localhost socket. Frames are a 4-byte big-endian length plus
UTF-8 JSON. stdout and stderr stay free; `print` is forwarded as a log frame.
User code is the body of an async function and keeps its own line numbers.
A read-only (scout) process installs an audit hook before that code runs.
The hook is best-effort: it is not a security boundary.
"""

import ast
import asyncio
import io
import json
import os
import socket
import struct
import sys
import threading
import traceback

_MAX_FRAME = 8 * 1024 * 1024
_sock = None
_write_lock = threading.Lock()
_loop = None
_pending = {}
_next_id = 1
_id_lock = threading.Lock()
_warned = set()
_tools = set()

_PROCESS = {
    "os.system",
    "os.exec",
    "os.spawn",
    "os.posix_spawn",
    "os.fork",
    "subprocess.Popen",
}
_SOCKET = {
    "socket.__new__",
    "socket.bind",
    "socket.connect",
    "socket.getaddrinfo",
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
}


def _recvn(sock, count):
    buf = bytearray()
    while len(buf) < count:
        chunk = sock.recv(count - len(buf))
        if not chunk:
            raise EOFError("control channel closed")
        buf += chunk
    return bytes(buf)


def _read_frame(sock):
    header = _recvn(sock, 4)
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > _MAX_FRAME:
        raise ValueError("control frame length %s" % length)
    return json.loads(_recvn(sock, length).decode("utf-8"))


def _write_frame(obj):
    data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
    if len(data) > _MAX_FRAME:
        raise ValueError("control frame is too large")
    packet = struct.pack(">I", len(data)) + data
    with _write_lock:
        _sock.sendall(packet)


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
            message = _read_frame(_sock)
        except Exception:
            _fail_pending("control channel closed")
            return
        if message.get("type") != "reply":
            continue
        ident = message.get("id")
        with _id_lock:
            future = _pending.pop(ident, None)
        if future is None or _loop is None:
            continue
        _loop.call_soon_threadsafe(_settle, future, message)


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
    if len(args) >= 2 and isinstance(args[1], str):
        mode = args[1]
    if len(args) >= 3 and isinstance(args[2], int):
        flags = args[2]
    if any(mark in mode for mark in ("w", "a", "x", "+")):
        return True
    write_bits = 0
    for bit in ("O_WRONLY", "O_RDWR", "O_APPEND", "O_CREAT", "O_TRUNC"):
        write_bits |= getattr(os, bit, 0)
    return bool(flags & write_bits)


def _install_hook(restricted, warn_process):
    """Best-effort guard. Audit hooks can be bypassed; they are not a sandbox."""

    def hook(event, args):
        process = (
            event in _PROCESS
            or event.startswith("os.exec")
            or event.startswith("os.spawn")
        )
        if process:
            if restricted:
                raise PermissionError("scout python blocked %s" % event)
            if warn_process:
                _warn(event)
            return
        if event.startswith("ctypes."):
            if restricted:
                raise PermissionError("scout python blocked ctypes")
            return
        if event in _SOCKET:
            if restricted:
                raise PermissionError("scout python blocked a network socket")
            return
        if event in _FILE_MUTATION or (event == "open" and _write_open(tuple(args))):
            if restricted:
                raise PermissionError(
                    "scout python blocked a file write, create, delete, or rename"
                )

    sys.addaudithook(hook)


def _format_exc(exc):
    if isinstance(exc, SyntaxError):
        line = exc.lineno or 0
        return "SyntaxError: %s (line %s)" % (exc.msg, line)
    rows = []
    for frame in traceback.extract_tb(exc.__traceback__):
        if frame.filename == "<user>":
            snippet = (frame.line or "").strip()
            rows.append(("line %s: %s" % (frame.lineno, snippet)).rstrip())
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


async def _run_user(code):
    try:
        result = await _exec_user(code)
    except Exception as exc:
        _write_frame(
            {
                "type": "done",
                "error": {"kind": "exception", "message": _format_exc(exc)},
            }
        )
        return
    try:
        json.dumps(result)
    except TypeError as exc:
        _write_frame(
            {
                "type": "done",
                "error": {
                    "kind": "invalid-output",
                    "message": "return value is not JSON: %s" % exc,
                },
            }
        )
        return
    _write_frame({"type": "done", "value": result})


def main():
    global _sock, _loop, _tools
    port = int(sys.argv[1])
    sock = socket.create_connection(("127.0.0.1", port), timeout=30)
    sock.settimeout(None)
    _sock = sock
    _write_frame({"type": "ready"})
    boot = _read_frame(sock)
    if boot.get("type") != "boot":
        raise SystemExit("expected boot")
    code = boot.get("code") or ""
    _tools = set(boot.get("tools") or [])
    restricted = bool(boot.get("restricted"))
    warn_process = bool(boot.get("warn_process"))
    sys.stdout = _Log()
    sys.stderr = _Log()
    # The event loop opens a self-pipe. Install the audit hook after that,
    # and before user code, so scout still blocks later sockets.
    _loop = asyncio.new_event_loop()
    asyncio.set_event_loop(_loop)
    threading.Thread(target=_reader, name="mycode-ptc", daemon=True).start()
    if restricted or warn_process:
        _install_hook(restricted, warn_process and not restricted)
    try:
        _loop.run_until_complete(_run_user(code))
    finally:
        _loop.close()


if __name__ == "__main__":
    main()
