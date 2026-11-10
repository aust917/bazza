"""Test-only raw TCP relay from an isolated WSL bridge to Windows loopback."""

import argparse
import contextlib
import ipaddress
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import threading
import time


READY, OPEN, DATA, FIN, CLOSE = range(5)
HEADER = struct.Struct("!BII")
CHUNK = 16_384
LIMIT = 16
DISTRO = "BazzaMeetAudioAuditQa"


def read_exact(stream, size):
    data = bytearray()
    while len(data) < size:
        chunk = stream.read(size - len(data))
        if not chunk:
            raise EOFError
        data.extend(chunk)
    return bytes(data)


class Relay:
    def __init__(self, source, sink, target_port=None):
        self.source = source
        self.sink = sink
        self.target_port = target_port
        self.connections = {}
        self.local_fins = set()
        self.remote_fins = set()
        self.lock = threading.Lock()
        self.write_lock = threading.Lock()
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.serial = 0

    def send(self, kind, connection, payload=b""):
        with self.write_lock:
            frame = memoryview(HEADER.pack(kind, connection, len(payload)) + payload)
            while frame:
                written = self.sink.write(frame)
                if not written:
                    raise BrokenPipeError("could not write relay frame")
                frame = frame[written:]
            self.sink.flush()

    def attach(self, connection, sock):
        sock.settimeout(1)
        with self.lock:
            if self.stop.is_set() or len(self.connections) >= LIMIT or connection in self.connections:
                sock.close()
                return False
            self.connections[connection] = sock
        return True

    def forget(self, connection):
        with self.lock:
            sock = self.connections.pop(connection, None)
            self.local_fins.discard(connection)
            self.remote_fins.discard(connection)
        if sock is not None:
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)
            sock.close()
        return sock is not None

    def mark_fin(self, connection, side):
        with self.lock:
            if connection not in self.connections:
                return
            side.add(connection)
            done = connection in self.local_fins and connection in self.remote_fins
        if done:
            self.forget(connection)

    def finish(self):
        self.stop.set()
        with self.lock:
            active = list(self.connections)
        for connection in active:
            self.forget(connection)

    def read_socket(self, connection, sock):
        try:
            while not self.stop.is_set():
                try:
                    data = sock.recv(CHUNK)
                except TimeoutError:
                    continue
                if not data:
                    self.send(FIN, connection)
                    self.mark_fin(connection, self.local_fins)
                    return
                self.send(DATA, connection, data)
        except OSError:
            if self.forget(connection):
                with contextlib.suppress(OSError):
                    self.send(CLOSE, connection)

    def start_reader(self, connection, sock):
        threading.Thread(target=self.read_socket, args=(connection, sock), daemon=True).start()

    def accept(self, sock):
        self.serial += 1
        connection = self.serial
        if self.attach(connection, sock):
            self.send(OPEN, connection)
            self.start_reader(connection, sock)

    def connect_target(self, connection):
        try:
            sock = socket.create_connection(("127.0.0.1", self.target_port), timeout=1)
        except OSError:
            self.send(CLOSE, connection)
            return
        if self.attach(connection, sock):
            self.start_reader(connection, sock)
        else:
            self.send(CLOSE, connection)

    def read_wire(self):
        try:
            while not self.stop.is_set():
                kind, connection, size = HEADER.unpack(read_exact(self.source, HEADER.size))
                if kind not in (READY, OPEN, DATA, FIN, CLOSE) or size > CHUNK or (kind != DATA and size):
                    raise ValueError("invalid relay frame")
                data = read_exact(self.source, size)
                if kind == READY and self.target_port is not None and connection == 0:
                    self.ready.set()
                elif kind == OPEN and self.target_port is not None and connection > 0:
                    self.connect_target(connection)
                elif kind == CLOSE:
                    self.forget(connection)
                elif kind in (DATA, FIN):
                    with self.lock:
                        sock = self.connections.get(connection)
                        remote_finished = connection in self.remote_fins
                    if sock is not None:
                        try:
                            if kind == DATA:
                                sock.sendall(data)
                            elif not remote_finished:
                                sock.shutdown(socket.SHUT_WR)
                                self.mark_fin(connection, self.remote_fins)
                        except OSError:
                            self.forget(connection)
                            self.send(CLOSE, connection)
                else:
                    raise ValueError("unexpected relay frame")
        except (EOFError, OSError, ValueError):
            self.finish()

    def start_wire_reader(self):
        threading.Thread(target=self.read_wire, daemon=True).start()


def linux_worker(address, port, seconds):
    relay = Relay(sys.stdin.buffer, sys.stdout.buffer)
    deadline = time.monotonic() + seconds + 5
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((address, port))
        listener.listen(LIMIT)
        listener.settimeout(0.25)
        relay.start_wire_reader()
        relay.send(READY, 0)
        try:
            while not relay.stop.is_set() and time.monotonic() < deadline:
                try:
                    sock, _ = listener.accept()
                except TimeoutError:
                    continue
                relay.accept(sock)
        finally:
            relay.finish()


def signal_path(path):
    if path is not None and (not path.is_absolute() or not path.parent.is_dir() or path.exists()):
        raise ValueError("signal paths must be absolute, fresh and in an existing directory")


def windows_main(args):
    if os.name != "nt":
        raise ValueError("launch the relay from Windows")
    signal_path(args.ready_file)
    signal_path(args.stop_file)
    if args.ready_file == args.stop_file:
        raise ValueError("ready and stop signal paths must differ")
    bridge = ipaddress.IPv4Address(args.bind)
    if not bridge.is_private or bridge.is_unspecified or bridge.is_multicast or bridge.is_link_local:
        raise ValueError("use the isolated bridge IPv4 address")
    script = Path(__file__).resolve()
    wsl_path = f"/mnt/{script.drive[0].lower()}/" + "/".join(script.parts[1:])
    wsl_exe = Path(os.environ["SystemRoot"]) / "System32" / "wsl.exe"
    command = [str(wsl_exe), "--distribution", args.distribution, "--exec", "python3", "-u",
               wsl_path, "--linux-worker", str(bridge), str(args.port), str(args.seconds)]
    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0,
                          creationflags=subprocess.CREATE_NO_WINDOW) as child:
        relay = Relay(child.stdout, child.stdin, args.target_port)
        relay.start_wire_reader()
        deadline = time.monotonic() + min(args.seconds, 30)
        try:
            while not relay.ready.wait(0.2):
                if relay.stop.is_set() or child.poll() is not None:
                    raise OSError("WSL relay exited before binding")
                if time.monotonic() >= deadline:
                    raise TimeoutError("WSL relay did not bind in 30 seconds")
            with args.ready_file.open("x", encoding="ascii") as signal:
                signal.write("ready\n")
            print("isolated TCP relay ready", flush=True)
            deadline = time.monotonic() + args.seconds
            while time.monotonic() < deadline:
                if args.stop_file is not None and args.stop_file.is_file():
                    return
                if relay.stop.wait(0.2) or child.poll() is not None:
                    raise OSError("WSL relay disconnected")
        finally:
            relay.finish()
            child.stdin.close()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=3)


def main():
    if len(sys.argv) == 5 and sys.argv[1] == "--linux-worker" and sys.platform.startswith("linux"):
        linux_worker(sys.argv[2], int(sys.argv[3]), int(sys.argv[4]))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--distribution", choices=[DISTRO], default=DISTRO)
    parser.add_argument("--bind", required=True)
    parser.add_argument("--port", type=int, default=56177)
    parser.add_argument("--target-port", type=int, default=56177)
    parser.add_argument("--seconds", type=int, default=900)
    parser.add_argument("--ready-file", type=Path, required=True)
    parser.add_argument("--stop-file", type=Path)
    args = parser.parse_args()
    if not (1024 <= args.port <= 65535 and 1024 <= args.target_port <= 65535 and 5 <= args.seconds <= 3600):
        parser.error("ports must be 1024–65535 and duration must be 5–3600 seconds")
    windows_main(args)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as exc:
        raise SystemExit(str(exc)) from None
