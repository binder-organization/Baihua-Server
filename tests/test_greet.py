from pathlib import Path

import pytest
import requests

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib


def _server_version() -> str:
    cargo_toml = Path(__file__).resolve().parent.parent / "Cargo.toml"
    with open(cargo_toml, "rb") as file:
        data = tomllib.load(file)
    return data["package"]["version"]


class TestGreet:
    @pytest.mark.smoke
    def test_greet_ok(self, session: requests.Session, base_url: str):
        resp = session.get(f"{base_url}/greet")
        assert resp.status_code == 200

        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["message"] == "Welcome to the test server."
        assert body["response_id"]
        assert body["data"]["server_identity"] == "baihua"
        assert body["data"]["server_version"] == _server_version()
        assert body["data"]["api_version"] == "v1"
        assert body["data"]["uptime_seconds"] >= 0
        assert body["data"]["health"] == {"status": "ok", "database": "reachable"}
