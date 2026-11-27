#!/usr/bin/env python3
"""Drive the xim_input example on a disposable X11 display and private D-Bus.

Requires Xvfb, Openbox, xdotool, and fcitx5+pinyin or ibus+rime. All input and
configuration are synthetic. Never run the session mode on a user's display.
"""
import argparse
import json
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import sys
import tempfile
import time


def command(*args, check=True):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=15, check=check).stdout.strip()


def wait_for(predicate, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"timeout waiting for {description}")


def session(args):
    output = Path(args.output)
    children = []
    files = []
    def start(name, *argv):
        stream = (output / f"{name}.log").open("w")
        files.append(stream)
        process = subprocess.Popen(argv, stdout=stream, stderr=stream)
        children.append(process)
        return process
    def start_ime():
        if args.engine == "fcitx":
            return start(f"fcitx-{len(children)}", "fcitx5", "-D", "--disable", "wayland,ibusfrontend", "-u", "classicui")
        command("gsettings", "set", "org.freedesktop.ibus.general", "use-global-engine", "true")
        # Rime advertises the default layout. `ibus engine` otherwise returns
        # failure after successfully selecting it when setxkbmap has no work.
        command("gsettings", "set", "org.freedesktop.ibus.general", "use-system-keyboard-layout", "true")
        command("gsettings", "set", "org.freedesktop.ibus.general", "preload-engines", "['rime']")
        return start(f"ibus-{len(children)}", "ibus-daemon", "--xim", "--replace", "--panel=disable", "--emoji-extension=disable", "--cache=refresh")
    def activate():
        if args.engine == "fcitx":
            wait_for(lambda: subprocess.run(["fcitx5-remote", "--check"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0, "fcitx service")
            command("fcitx5-remote", "-s", "pinyin")
            command("fcitx5-remote", "-o")
        else:
            def select_ibus():
                result = subprocess.run(["ibus", "engine", "rime"], text=True, capture_output=True, timeout=15)
                (output / "ibus-activation.log").write_text(f"exit={result.returncode}\n" + result.stdout + result.stderr)
                if result.returncode and not (output / "ibus-engines.log").exists():
                    (output / "ibus-engines.log").write_text(command("ibus", "list-engine", "--name-only", check=False))
                return result.returncode == 0
            wait_for(select_ibus, "ibus Rime", 45)
    def counters():
        content = (output / "probe.log").read_text(errors="replace")
        matches = re.findall(r"IME_PROBE id=(\d+) committed=(\d+) chinese=(\d+) preedits=(\d+)", content)
        result = {}
        for field, committed, chinese, preedits in matches:
            result[int(field)] = {"committed": int(committed), "chinese": int(chinese), "preedits": int(preedits)}
        return result
    def cjk_count(field):
        return counters().get(field, {}).get("chinese", 0)
    def geometry(window):
        return {key: int(value) for key, value in re.findall(r"^(X|Y|WIDTH|HEIGHT)=(-?\d+)$", command("xdotool", "getwindowgeometry", "--shell", window), re.M)}
    def candidate_position(window, anchor):
        command("xdotool", "windowactivate", "--sync", window)
        activate()
        command("xdotool", "key", "Escape")
        command("xdotool", "type", "--clearmodifiers", "--delay", "100", "nihao")
        candidate = wait_for(lambda: command("xdotool", "search", "--onlyvisible", "--name", "^Fcitx5 Input Window$", check=False).splitlines(), "candidate window")[0]
        position = geometry(candidate)
        parent = geometry(window)
        expected = (parent["X"] + anchor[0] + 8, parent["Y"] + anchor[1] + 24)
        if abs(position["X"] - expected[0]) > 60 or abs(position["Y"] - expected[1]) > 60:
            raise AssertionError(f"candidate far from caret: {position}, expected={expected}")
        command("xdotool", "key", "Escape")
        return position
    def type_chinese(window, field):
        command("xdotool", "windowactivate", "--sync", window)
        activate()
        before = cjk_count(field)
        command("xdotool", "key", "Escape")
        command("xdotool", "type", "--clearmodifiers", "--delay", "100", "nihao")
        command("xdotool", "key", "space")
        wait_for(lambda: cjk_count(field) > before, "Chinese commit", 12)
    results = {"engine": args.engine, "late_start": args.late_start, "checks": []}
    try:
        start("openbox", "openbox")
        ime = None if args.late_start else start_ime()
        probe = start("probe", args.probe, "300")
        windows = [wait_for(lambda i=i: command("xdotool", "search", "--name", f"^GPUI XIM probe {i}$", check=False).splitlines(), f"probe window {i}")[0] for i in range(2)]
        command("xdotool", "windowactivate", "--sync", windows[0])
        if args.late_start:
            time.sleep(2)
            ime = start_ime()
            activate()
            time.sleep(6)
        else:
            time.sleep(2)
        type_chinese(windows[0], 0)
        results["checks"].append("initial Chinese commit")
        if args.engine == "fcitx":
            positions = [candidate_position(windows[0], (80, 80))]
            command("xdotool", "mousemove", "--window", windows[0], "180", "140", "click", "1")
            positions.append(candidate_position(windows[0], (180, 140)))
            results["candidate_positions"] = positions
            results["checks"].append("candidate tracks initial and moved caret")
        for i in range(100):
            command("xdotool", "windowactivate", "--sync", windows[i % 2])
            time.sleep(0.025)
        type_chinese(windows[0], 0)
        type_chinese(windows[1], 1)
        results["checks"].append("100 focus switches, both windows commit")
        ime.terminate()
        ime.wait(timeout=10)
        time.sleep(1)
        ime = start_ime()
        activate()
        time.sleep(6)
        if probe.poll() is not None:
            raise RuntimeError("probe exited during input-method restart")
        type_chinese(windows[1], 1)
        results["checks"].append("input method restart without application restart")
        deadline = time.monotonic() + args.duration
        while time.monotonic() < deadline:
            type_chinese(windows[1], 1)
            time.sleep(0.5)
        results["checks"].append(f"continuous input for {args.duration} seconds")
        results["counters"] = counters()
        results["passed"] = True
    except Exception as error:
        results["passed"] = False
        results["error"] = str(error)
        raise
    finally:
        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        for stream in files:
            stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", required=True)
    parser.add_argument("--engine", choices=["fcitx", "ibus"], required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--duration", type=int, default=60)
    parser.add_argument("--late-start", action="store_true")
    parser.add_argument("--on-the-spot", action="store_true")
    parser.add_argument("--session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 0 <= args.duration <= 120:
        parser.error("duration must be between 0 and 120 seconds")
    if args.session:
        session(args)
        return
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nyaterm-xim-") as temporary:
        root = Path(temporary)
        environment = os.environ.copy()
        for variable, directory in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "runtime")]:
            path = root / directory
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        environment.pop("WAYLAND_DISPLAY", None)
        environment.update(XMODIFIERS=f"@im={args.engine}", XDG_SESSION_TYPE="x11", GDK_BACKEND="x11", LC_ALL="C.UTF-8")
        profile = root / "config/fcitx5/profile"
        profile.parent.mkdir(parents=True)
        profile.write_text("[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=pinyin\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=\n\n[Groups/0/Items/1]\nName=pinyin\nLayout=\n\n[GroupOrder]\n0=Default\n")
        if args.on_the_spot:
            xim_config = root / "config/fcitx5/conf/xim.conf"
            xim_config.parent.mkdir(parents=True)
            xim_config.write_text("UseOnTheSpot=True\n")
        rime = root / "config/ibus/rime"
        rime.mkdir(parents=True)
        (rime / "default.custom.yaml").write_text('patch:\n  schema_list:\n    - schema: luna_pinyin\n')
        # Let Xvfb choose an unused display; never touch an existing user server.
        read_fd, write_fd = os.pipe()
        server_log = (output / "xvfb.log").open("w")
        server = subprocess.Popen(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "1280x800x24", "-nolisten", "tcp"], pass_fds=(write_fd,), stdout=server_log, stderr=server_log)
        os.close(write_fd)
        try:
            with os.fdopen(read_fd) as reader:
                if not select.select([reader], [], [], 10)[0]:
                    raise RuntimeError("Xvfb did not provide a display number")
                display = reader.readline().strip()
                if not display.isdigit():
                    raise RuntimeError("Xvfb failed to start")
                environment["DISPLAY"] = ":" + display
            argv = [sys.executable, str(Path(__file__).resolve()), "--session", "--probe", str(Path(args.probe).resolve()), "--engine", args.engine, "--output", str(output), "--duration", str(args.duration)]
            if args.late_start:
                argv.append("--late-start")
            if args.on_the_spot:
                argv.append("--on-the-spot")
            runner = subprocess.Popen(["dbus-run-session", "--", *argv], env=environment, start_new_session=True)
            try:
                status = runner.wait(timeout=240)
                if status:
                    raise subprocess.CalledProcessError(status, runner.args)
            finally:
                if runner.poll() is None:
                    os.killpg(runner.pid, signal.SIGTERM)
                    try:
                        runner.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(runner.pid, signal.SIGKILL)
                        runner.wait()
        finally:
            server.terminate()
            server.wait(timeout=10)
            server_log.close()
    print((output / "results.json").read_text())


if __name__ == "__main__":
    main()
