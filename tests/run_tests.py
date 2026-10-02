#!/usr/bin/env python3
"""Run the integration suite against an isolated server and database."""

import argparse
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from collections import deque
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
TEST_COMPOSE_FILE = PROJECT_ROOT / "tests" / "compose.test.yml"
LOG_DIRECTORY = PROJECT_ROOT / "tests" / "logs"
EXPECTED_TEST_CLASSES = (
    "tests/test_chat.py::ChatTest::",
    "tests/test_group_chat.py::TestGroupChat::",
    "tests/test_encrypted_chat.py::EncryptedChatTest::",
    "tests/test_room_requests.py::RoomRequestChatTest::",
)


def log(message):
    print(f"[tests-runner] {message}", flush=True)


def run_checks():
    checks = (
        ("formatting", ["cargo", "fmt", "--check"]),
        ("linting", ["cargo", "clippy", "--locked", "--", "-D", "warnings"]),
    )
    for label, command in checks:
        log(f"Checking {label}...")
        result = subprocess.run(command, cwd=PROJECT_ROOT, capture_output=True, text=True)
        if result.returncode:
            print(result.stdout, end="")
            print(result.stderr, end="", file=sys.stderr)
            return False
    return True


def verify_collection():
    result = subprocess.run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q", "tests"],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        return False
    missing_classes = [
        class_name for class_name in EXPECTED_TEST_CLASSES
        if class_name not in result.stdout
    ]
    if missing_classes:
        log(f"Test collection missed: {', '.join(missing_classes)}")
        return False
    log(result.stdout.splitlines()[-1])
    return True


def choose_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class TestRun:
    def __init__(self):
        self.identifier = uuid.uuid4().hex[:12]
        self.project_name = f"baihua-test-{self.identifier}"
        self.temporary_directory = Path(
            tempfile.mkdtemp(prefix=f"{self.project_name}-")
        )
        LOG_DIRECTORY.mkdir(parents=True, exist_ok=True)
        self.server_log_path = LOG_DIRECTORY / f"server_{self.identifier}.log"
        self.server_log_file = None
        self.server_process = None
        self.compose_started = False
        self.server_url = None
        self.environment = os.environ.copy()
        self.environment.update({
            "POSTGRES_USER": f"baihua_test_{self.identifier}",
            "POSTGRES_PASSWORD": uuid.uuid4().hex + uuid.uuid4().hex,
            "POSTGRES_DB": f"baihua_test_{self.identifier}",
            "JWT_SECRET": uuid.uuid4().hex + uuid.uuid4().hex,
            "BAIHUA_ENCRYPTED_GRACE_PERIOD_SECS": "5",
        })

    def compose_command(self, *arguments):
        return [
            "docker", "compose", "-f", str(TEST_COMPOSE_FILE),
            "-p", self.project_name, *arguments,
        ]

    def compose(self, *arguments, timeout=120):
        result = subprocess.run(
            self.compose_command(*arguments),
            cwd=PROJECT_ROOT,
            env=self.environment,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        if result.returncode:
            raise RuntimeError(result.stderr or result.stdout)
        return result.stdout.strip()

    def mapped_port(self, service, internal_port, profile=None):
        arguments = [] if profile is None else ["--profile", profile]
        output = self.compose(*arguments, "port", service, str(internal_port))
        return int(output.rsplit(":", 1)[-1])

    def start_database(self):
        self.compose_started = True
        self.compose("up", "-d", "--wait", "database")
        self.environment["POSTGRES_HOST"] = "127.0.0.1"
        self.environment["POSTGRES_PORT"] = str(self.mapped_port("database", 5432))
        log(f"Isolated database: {self.project_name}")

    def use_explicit_local_database(self):
        required = (
            "POSTGRES_HOST", "POSTGRES_PORT", "POSTGRES_USER",
            "POSTGRES_PASSWORD", "POSTGRES_DB",
        )
        if any(not os.environ.get(name) for name in required):
            raise RuntimeError(
                "Local mode requires explicit PostgreSQL connection variables."
            )
        if os.environ["POSTGRES_HOST"] not in ("localhost", "127.0.0.1", "::1"):
            raise RuntimeError("Local mode only accepts a local PostgreSQL host.")
        if not os.environ["POSTGRES_DB"].startswith("baihua_test_"):
            raise RuntimeError("Local mode requires a baihua_test_ database name.")
        self.environment.update({name: os.environ[name] for name in required})
        log(f"Using explicitly selected test database: {self.environment['POSTGRES_DB']}")

    def write_server_configuration(self, port):
        app_directory = self.temporary_directory / "app"
        app_directory.mkdir()
        (app_directory / "config.toml").write_text(
            f'[web]\nhost = "127.0.0.1"\nport = {port}\n'
            "[logs]\n[database]\n[user]\n",
            encoding="utf-8",
        )
        self.environment["BAIHUA_DIR"] = str(app_directory)

    def start_local_server(self):
        build = subprocess.run(
            ["cargo", "build", "--locked"],
            cwd=PROJECT_ROOT,
            capture_output=True,
            text=True,
        )
        if build.returncode:
            raise RuntimeError(build.stderr or build.stdout)

        binary = self.temporary_directory / "baihua-server"
        shutil.copy2(PROJECT_ROOT / "target" / "debug" / "baihua-server", binary)
        (self.temporary_directory / "migrations").symlink_to(
            PROJECT_ROOT / "migrations", target_is_directory=True
        )
        port = choose_port()
        self.write_server_configuration(port)
        self.environment["BAIHUA_ENV"] = "production"
        self.server_log_file = self.server_log_path.open("w", encoding="utf-8")
        self.server_process = subprocess.Popen(
            [str(binary)],
            cwd=PROJECT_ROOT,
            env=self.environment,
            stdin=subprocess.DEVNULL,
            stdout=self.server_log_file,
            stderr=subprocess.STDOUT,
        )
        self.server_url = f"http://127.0.0.1:{port}"
        self.wait_until_healthy()

    def start_container_server(self):
        self.compose("--profile", "container", "up", "-d", "--build", "--wait", "server", timeout=1200)
        port = self.mapped_port("server", 2424, profile="container")
        self.server_url = f"http://127.0.0.1:{port}"
        self.wait_until_healthy()

    def wait_until_healthy(self):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.server_process is not None and self.server_process.poll() is not None:
                raise RuntimeError(self.server_log_tail())
            try:
                with urllib.request.urlopen(f"{self.server_url}/health", timeout=2) as response:
                    if response.status == 200:
                        log(f"Server ready: {self.server_url}")
                        return
            except (urllib.error.URLError, TimeoutError, OSError):
                pass
            time.sleep(0.5)
        raise RuntimeError(f"Server did not become healthy. {self.server_log_tail()}")

    def server_log_tail(self):
        if not self.server_log_path.exists():
            return "No server log was created."
        with self.server_log_path.open(encoding="utf-8", errors="replace") as log_file:
            return "\n".join(deque(log_file, maxlen=30))

    def run_tests(self, arguments):
        self.environment["BAIHUA_TEST_BASE_URL"] = self.server_url
        selected = arguments.test or ["tests"]
        command = [sys.executable, "-m", "pytest", *selected, "-v", "--tb=short"]
        if arguments.smoke:
            command.extend(["-m", "smoke"])
        return subprocess.run(
            command, cwd=PROJECT_ROOT, env=self.environment
        ).returncode

    def cleanup(self):
        if self.server_process is not None and self.server_process.poll() is None:
            self.server_process.terminate()
            try:
                self.server_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.server_process.kill()
                self.server_process.wait()
        if self.server_log_file is not None:
            self.server_log_file.close()
        if self.compose_started and self.project_name.startswith("baihua-test-"):
            try:
                self.compose("down", "-v", "--remove-orphans")
            except (RuntimeError, subprocess.TimeoutExpired) as error:
                log(f"Could not clean up {self.project_name}: {error}")
        shutil.rmtree(self.temporary_directory)

    def describe_kept_resources(self):
        log(f"Kept server: {self.server_url}")
        log(f"Kept temporary directory: {self.temporary_directory}")
        if self.compose_started:
            log(f"Kept container project: {self.project_name}")
            log("Clean it with: " + " ".join(self.compose_command("down", "-v")))
        if self.server_log_file is not None:
            self.server_log_file.close()


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docker", action="store_true", help="Run the server in a container")
    parser.add_argument("--local", action="store_true", help="Use an explicit local test database")
    parser.add_argument("--keep", action="store_true", help="Keep resources for inspection")
    parser.add_argument("--skip-checks", action="store_true", help="Skip formatting and linting checks")
    parser.add_argument("--smoke", action="store_true", help="Run critical smoke scenarios")
    parser.add_argument("--test", action="append", help="Select a test path or node; repeat as needed")
    arguments = parser.parse_args()
    if arguments.docker and arguments.local:
        parser.error("--docker and --local cannot be combined")
    if sys.version_info < (3, 10):
        log("Python 3.10 or newer is required.")
        return 1
    if not arguments.skip_checks and not run_checks():
        return 1
    if not verify_collection():
        return 1

    test_run = TestRun()
    completed = False
    try:
        if arguments.local:
            test_run.use_explicit_local_database()
        else:
            test_run.start_database()
        if arguments.docker:
            test_run.start_container_server()
        else:
            test_run.start_local_server()
        result = test_run.run_tests(arguments)
        completed = True
        return result
    except KeyboardInterrupt:
        log("Interrupted.")
        return 130
    except (OSError, RuntimeError, subprocess.TimeoutExpired, ValueError) as error:
        log(f"Test environment failed: {error}")
        return 1
    finally:
        if arguments.keep and completed:
            test_run.describe_kept_resources()
        else:
            test_run.cleanup()


if __name__ == "__main__":
    sys.exit(run())
