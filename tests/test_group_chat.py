import json
import time
import uuid

import pytest
import requests
import websocket


# ── helpers (module-level, mirroring test_chat.py style) ──────────────

def _unique(name: str) -> str:
    """Generate a unique username/email to avoid cross-run conflicts."""
    suffix = uuid.uuid4().hex[:8]
    return f"{name}_{suffix}"


def _register_user(
    session: requests.Session, base_url: str, *, prefix: str = "grpuser"
) -> dict:
    """Register a user with a unique name and return the user data dict."""
    uname = _unique(prefix)
    resp = session.post(
        f"{base_url}/api/v1/user/register",
        json={
            "username": uname,
            "email": f"{uname}@example.com",
            "password": "P@ssw0rd!",
        },
    )
    assert resp.status_code == 201, resp.text
    return resp.json()["data"]["user"]


def _register_and_login(
    session: requests.Session, base_url: str, *, prefix: str = "grpuser"
) -> tuple[str, dict]:
    """Register + login a unique user.  Returns (token, user_data)."""
    user = _register_user(session, base_url, prefix=prefix)
    login_resp = session.post(
        f"{base_url}/api/v1/user/login",
        json={"username": user["username"], "password": "P@ssw0rd!"},
    )
    assert login_resp.status_code == 200, login_resp.text
    body = login_resp.json()
    assert body["code"] == "SUCCESS"
    return body["data"]["token"], user


# ── WebSocket helpers ──────────────────────────────────────────────

def _ws_connect(ws_base: str, token: str, timeout: int = 10) -> websocket.WebSocket:
    """Create a WebSocket connection with JWT token in Authorization header."""
    return websocket.create_connection(
        f"{ws_base}/websocket",
        header={"authorization": f"Bearer {token}"},
        timeout=timeout,
    )


def _recv(ws: websocket.WebSocket, timeout: int = 10) -> dict:
    """Receive one WebSocket message, parse JSON, with timeout."""
    ws.settimeout(timeout)
    raw = ws.recv()
    return json.loads(raw)


def _recv_until(
    ws: websocket.WebSocket, expected_type: str, timeout: int = 10
) -> dict:
    """Consume messages until one of the expected type arrives. Skips others."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        ws.settimeout(remaining)
        try:
            raw = ws.recv()
            msg = json.loads(raw)
            if msg["type"] == expected_type:
                return msg
        except websocket.WebSocketTimeoutException:
            break
    pytest.fail(f"Did not receive '{expected_type}' within {timeout}s")


# ── shared state (class-level, survives across test methods) ──────────

class _State:
    """Mutable bag so class-level attributes are easily reassignable."""


# ── tests ─────────────────────────────────────────────────────────────

class TestGroupChat:
    """15 scenarios for the /api/v1/chat group room feature."""

    @pytest.fixture(autouse=True)
    def _prepare_group_room(self, request, session: requests.Session, base_url: str):
        if request.node.name in {
            "test_g1_create_group_room",
            "test_g15_adjacent_surface_regression",
        }:
            return

        self.user_b = _register_user(session, base_url, prefix="grproomb")
        self.user_c = _register_user(session, base_url, prefix="grproomc")
        self.token_admin, self.user_admin = _register_and_login(
            session, base_url, prefix="grprooma"
        )
        usernames = [self.user_b["username"]]
        if request.node.name != "test_g12_group_in_room_list":
            usernames.append(self.user_c["username"])
        response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "test-group", "usernames": usernames},
            headers=self._auth(self.token_admin),
        )
        assert response.status_code == 201, response.text
        self.room_id = response.json()["data"]["id"]

        if request.node.name in {
            "test_g6_list_members",
            "test_g7_get_room_detail",
            "test_g9_admin_kick",
        }:
            self.user_d = _register_user(session, base_url, prefix="grproomd")
            response = session.post(
                f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
                json={"usernames": [self.user_d["username"]]},
                headers=self._auth(self.token_admin),
            )
            assert response.status_code == 200, response.text

    @staticmethod
    def _auth(token: str) -> dict:
        return {"Authorization": f"Bearer {token}"}

    # ── G1 ────────────────────────────────────────────────────────────

    @pytest.mark.smoke
    def test_g1_create_group_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G1 – Create group room (happy path).

        Register user A (admin), user B and user C (members).
        A creates a group room with name and usernames [B, C].
        Expect 201, is_group=true, name matches."""
        # Register members first (no login needed for registration)
        self.user_b = _register_user(session, base_url, prefix="grpb")
        self.user_c = _register_user(session, base_url, prefix="grpc")
        # Admin registers and logs in
        self.token_admin, self.user_admin = _register_and_login(
            session, base_url, prefix="grpa"
        )

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "test-group",
                "usernames": [
                    self.user_b["username"],
                    self.user_c["username"],
                ],
            },
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 201, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]

        # room id must be a valid UUID
        room_id = data["id"]
        uuid.UUID(room_id)

        # verify group room fields
        assert data["is_group"] is True
        assert data["name"] == "test-group"
        assert data["created_by"] == self.user_admin["id"]

        # exactly 3 members containing all 3 users
        members = data["members"]
        assert len(members) == 3
        assert self.user_admin["id"] in members
        assert self.user_b["id"] in members
        assert self.user_c["id"] in members


    # ── G2 ────────────────────────────────────────────────────────────

    def test_g2_duplicate_username_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G2 – Duplicate usernames in group creation are rejected."""
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "dup-group",
                "usernames": [
                    self.user_b["username"],
                    self.user_b["username"],  # duplicate
                ],
            },
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    # ── G3 ────────────────────────────────────────────────────────────

    def test_g3_group_room_validation(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G3 – Validation for missing/empty name and usernames."""
        headers = self._auth(self.token_admin)

        # missing name
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "usernames": ["someone"]},
            headers=headers,
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

        # empty name
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "", "usernames": ["someone"]},
            headers=headers,
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

        # empty usernames list
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "test", "usernames": []},
            headers=headers,
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

        # missing usernames
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "test"},
            headers=headers,
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    # ── G4 ────────────────────────────────────────────────────────────

    def test_g4_add_members(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G4 – Admin adds new members to the group room.

        Register user D, then admin adds D via POST /members.
        Expect 200, added_count == 1."""
        self.user_d = _register_user(session, base_url, prefix="grpd")

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            json={"usernames": [self.user_d["username"]]},
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]
        assert data["added_count"] == 1
        assert data["added"][0]["username"] == self.user_d["username"]

    # ── G5 ────────────────────────────────────────────────────────────

    def test_g5_non_admin_cannot_add_members(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G5 – Non-admin member cannot add members → 403 FORBIDDEN_ERROR."""
        # login as user_b (regular member, registered in G1)
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={
                "username": self.user_b["username"],
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 200, resp.text
        token_b = resp.json()["data"]["token"]

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            json={"usernames": ["someuser"]},
            headers=self._auth(token_b),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

    # ── G6 ────────────────────────────────────────────────────────────

    def test_g6_list_members(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G6 – List members of group room with roles.

        Expect 4 members (A admin, B member, C member, D member)
        with correct role assignments."""
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]
        assert data["count"] == 4  # A, B, C, D
        members = data["members"]
        assert len(members) == 4

        # verify member structure
        admin_found = False
        for m in members:
            assert "user_id" in m
            assert "username" in m
            assert "role" in m
            assert "joined_at" in m
            if m["user_id"] == self.user_admin["id"]:
                assert m["role"] == "admin"
                admin_found = True
        assert admin_found, "Admin not found in members list"

    # ── G7 ────────────────────────────────────────────────────────────

    def test_g7_get_room_detail(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G7 – Get detailed room info with member details.

        Verify is_group, name, member_count, and member structure."""
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}",
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]

        assert data["id"] == self.room_id
        assert data["name"] == "test-group"
        assert data["is_group"] is True
        assert data["member_count"] == 4

        members = data["members"]
        assert len(members) == 4
        for m in members:
            assert "user_id" in m
            assert "username" in m
            assert "role" in m
            assert "joined_at" in m

    # ── G8 ────────────────────────────────────────────────────────────

    def test_g8_self_leave(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G8 – Member leaves the group room.

        User C leaves. Expect 200, room_deleted=false.
        Verify C is no longer in the member list."""
        # login as user_c
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={
                "username": self.user_c["username"],
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 200, resp.text
        token_c = resp.json()["data"]["token"]

        resp = session.delete(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members/{self.user_c['id']}",
            headers=self._auth(token_c),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]
        assert data["left_user_id"] == self.user_c["id"]
        assert data["room_deleted"] is False

        # verify user_c is no longer a member
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            headers=self._auth(self.token_admin),
        )
        member_ids = [m["user_id"] for m in resp.json()["data"]["members"]]
        assert self.user_c["id"] not in member_ids

    # ── G9 ────────────────────────────────────────────────────────────

    def test_g9_admin_kick(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G9 – Admin kicks a member from the group.

        Admin removes user D. Expect 200, removed_user_id matches.
        Verify D is no longer in the member list."""
        resp = session.delete(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members/{self.user_d['id']}",
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]
        assert data["removed_user_id"] == self.user_d["id"]

        # verify user_d is no longer a member
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            headers=self._auth(self.token_admin),
        )
        member_ids = [m["user_id"] for m in resp.json()["data"]["members"]]
        assert self.user_d["id"] not in member_ids

    # ── G10 ───────────────────────────────────────────────────────────

    def test_g10_non_member_access_denied(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G10 – Non-member cannot access group room resources.

        Register user X (not in the group). Verify 403 on:
        - GET /messages
        - GET /rooms/{id}
        - GET /rooms/{id}/members"""
        token_x, _user_x = _register_and_login(session, base_url, prefix="grpx")

        # get messages
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/messages",
            headers=self._auth(token_x),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

        # get room detail
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}",
            headers=self._auth(token_x),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

        # list members
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/members",
            headers=self._auth(token_x),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

    # ── G11 ───────────────────────────────────────────────────────────

    def test_g11_send_message_in_group(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G11 – Send a message in the group room via WebSocket.

        Expect message_sent ack, sender_id matches admin, content matches, room_id matches."""
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, self.token_admin)
        try:
            _recv(ws)  # consume "connected"
            ws.send(
                json.dumps({
                    "type": "send_message",
                    "data": {
                        "room_id": self.room_id,
                        "content": "hello from group",
                    },
                })
            )
            msg = _recv_until(ws, "message_sent")
            data = msg["data"]
            assert data["sender_id"] == self.user_admin["id"]
            assert data["content"] == "hello from group"
            assert data["room_id"] == self.room_id
        finally:
            ws.close()

    # ── G12 ───────────────────────────────────────────────────────────

    def test_g12_group_in_room_list(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G12 – Group room appears in authenticated user's room list.

        Verify is_group=true, name matches, member_count correct
        (2 remaining after G8 leave + G9 kick)."""
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 200, resp.text

        rooms = resp.json()["data"]["rooms"]
        group_rooms = [r for r in rooms if r["id"] == self.room_id]
        assert len(group_rooms) == 1

        room = group_rooms[0]
        assert room["is_group"] is True
        assert room["name"] == "test-group"
        assert room["member_count"] == 2  # A + B (C left, D kicked)

    # ── G13 ───────────────────────────────────────────────────────────

    def test_g13_target_user_not_found(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G13 – Creating a group with non-existent username → 404."""
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "ghost-group",
                "usernames": ["nonexistent_user_12345"],
            },
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 404, resp.text
        assert resp.json()["code"] == "NOT_FOUND_ERROR"

    # ── G14 ───────────────────────────────────────────────────────────

    def test_g14_cannot_add_self_as_member(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G14 – Cannot include self in usernames list.

        The creator is automatically included; adding yourself
        explicitly should be rejected."""
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "self-group",
                "usernames": [self.user_admin["username"]],
            },
            headers=self._auth(self.token_admin),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    # ── G15 ───────────────────────────────────────────────────────────

    def test_g15_adjacent_surface_regression(
        self, session: requests.Session, base_url: str
    ) -> None:
        """G15 – Adjacent surface regression.

        Sanity-check that unrelated endpoints still work after group
        chat changes."""
        # greet
        resp = session.get(f"{base_url}/greet")
        assert resp.status_code == 200

        # health
        resp = session.get(f"{base_url}/health")
        assert resp.status_code == 200

        # register
        uname = _unique("g15reg")
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 201, resp.text

        # login (full flow via helper)
        token, _user = _register_and_login(session, base_url, prefix="g15log")
        assert token is not None
