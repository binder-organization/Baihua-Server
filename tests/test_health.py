import pytest
import requests


class TestHealth:
    @pytest.mark.smoke
    def test_health_ok(self, session: requests.Session, base_url: str):
        resp = session.get(f"{base_url}/health")
        assert resp.status_code == 200

        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["status"] == "ok"
