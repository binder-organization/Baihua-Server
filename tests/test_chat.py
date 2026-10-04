import json
import time
import uuid
from datetime import datetime

import pytest
import requests
import websocket


# ── helpers (module-level, mirroring test_user.py style) ──────────────

def _unique(name: str) -> str:
    """Generate a unique username/email to avoid cross-run conflicts."""
    suffix = uuid.uuid4().hex[:8]
    return f"{name}_{suffix}"


def _register_user(
    session: requests.Session, base_url: str, *, prefix: str = "chatuser"
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
    session: requests.Session, base_url: str, *, prefix: str = "chatuser"
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

# ── tests ─────────────────────────────────────────────────────────────

class ChatTest:
    """21 scenarios for the /api/v1/chat feature (HTTP + WebSocket)."""

    @pytest.fixture(autouse=True)
    def _prepare_shared_room(self, request, session: requests.Session, base_url: str):
        room_scenarios = {
            "test_s2_idempotent_create",
            "test_s3_send_message",
            "test_s4_get_messages_pagination",
            "test_s6_not_room_member",
            "test_s7_target_user_not_found",
            "test_s8_list_rooms",
            "test_s9_search_users",
            "test_s17_websocket_rooms_listed_on_connect",
            "test_s18_websocket_new_message_broadcast",
            "test_s19_websocket_typing_indicator",
            "test_s20_websocket_user_online_offline",
            "test_s21_websocket_leave_stops_messages",
        }
        if request.node.name not in room_scenarios:
            return

        self.token_a, self.user_a = _register_and_login(
            session, base_url, prefix="chatrooma"
        )
        self.token_b, self.user_b = _register_and_login(
            session, base_url, prefix="chatroomb"
        )
        request_response = session.post(
            f"{base_url}/api/v1/chat/rooms/requests",
            json={
                "receiver_id": self.user_b["id"],
                "message": "hello",
                "is_encrypted": False,
            },
            headers=self._auth(self.token_a),
        )
        assert request_response.status_code == 201, request_response.text
        request_id = request_response.json()["data"]["request_id"]
        accept_response = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=self._auth(self.token_b),
        )
        assert accept_response.status_code == 200, accept_response.text
        self.room_id = accept_response.json()["data"]["room"]["id"]

        if request.node.name == "test_s4_get_messages_pagination":
            socket = _ws_connect(base_url.replace("http", "ws"), self.token_a)
            try:
                _recv(socket)
                socket.send(json.dumps({
                    "type": "send_message",
                    "data": {"room_id": self.room_id, "content": "hello"},
                }))
                _recv_until(socket, "message_sent")
            finally:
                socket.close()

    @staticmethod
    def _auth(token: str) -> dict:
        return {"Authorization": f"Bearer {token}"}

    # ── S1 ────────────────────────────────────────────────────────────

    def test_s1_create_private_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S1 – Create private room via room request (happy path).

        Register user A (login) and user B (login).  A sends B a room
        request, B accepts it, and the server creates the private room.
        Expect 201 on the request, 200 on the accept, a UUID room id,
        and exactly 2 members."""
        token_a, user_a = _register_and_login(session, base_url, prefix="chata")
        token_b, user_b = _register_and_login(session, base_url, prefix="chatb")

        # A sends B a private room request
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests",
            json={
                "receiver_id": user_b["id"],
                "message": "hello",
                "is_encrypted": False,
            },
            headers=self._auth(token_a),
        )
        assert resp.status_code == 201, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]
        assert data["status"] == "pending"
        request_id = data["request_id"]

        # B sees the request in their pending list
        pending_resp = session.get(
            f"{base_url}/api/v1/chat/rooms/requests/pending",
            headers=self._auth(token_b),
        )
        assert pending_resp.status_code == 200, pending_resp.text
        pending_data = pending_resp.json()["data"]
        pending_ids = [request["id"] for request in pending_data["requests"]]
        assert request_id in pending_ids

        # B accepts the request and the server creates the private room
        accept_resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=self._auth(token_b),
        )
        assert accept_resp.status_code == 200, accept_resp.text

        accept_body = accept_resp.json()
        assert accept_body["code"] == "SUCCESS"
        assert accept_body["data"]["status"] == "accepted"
        room = accept_body["data"]["room"]

        # room id must be a valid UUID
        room_id = room["id"]
        uuid.UUID(room_id)

        # exactly 2 members containing both A and B
        members = room["members"]
        assert len(members) == 2
        assert user_a["id"] in members
        assert user_b["id"] in members


    # ── S2 ────────────────────────────────────────────────────────────

    def test_s2_idempotent_create(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S2 – Idempotent create.

        Sending the same room-creation request again returns 200 and the
        same room id."""
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": self.user_b["username"]},
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 200, resp.text
        assert resp.json()["data"]["id"] == self.room_id

    # ── S3 ────────────────────────────────────────────────────────────

    def test_s3_send_message(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S3 – Send a message to the room via WebSocket.

        Expect message_sent ack, sender_id matches A, content matches, room_id matches."""
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, self.token_a)
        try:
            _recv(ws)  # consume "connected"
            ws.send(
                json.dumps({
                    "type": "send_message",
                    "data": {
                        "room_id": self.room_id,
                        "content": "hello",
                    },
                })
            )
            msg = _recv_until(ws, "message_sent")
            data = msg["data"]
            assert data["sender_id"] == self.user_a["id"]
            assert data["content"] == "hello"
            assert data["room_id"] == self.room_id
        finally:
            ws.close()

    # ── S4 ────────────────────────────────────────────────────────────

    def test_s4_get_messages_pagination(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S4 – Get messages with cursor-based pagination.

        1.  Send 2 more messages (total 3 with S3's) for a proper
            pagination run.
        2.  Page 1: limit=2 → 2 messages, has_more=True, next_cursor set.
            Messages must be newest-first.
        3.  Page 2: limit=2 & before=next_cursor → 1 message,
            has_more=False."""
        # send 2 additional messages so we have exactly 3 in the room
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, self.token_a)
        try:
            _recv(ws)  # consume "connected"
            for content in ("world", "again"):
                ws.send(
                    json.dumps({
                        "type": "send_message",
                        "data": {
                            "room_id": self.room_id,
                            "content": content,
                        },
                    })
                )
                _recv_until(ws, "message_sent")  # consume ack
        finally:
            ws.close()

        # page 1 ───────────────────────────────────────────────────────
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/messages",
            params={"limit": 2},
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        data = body["data"]
        messages = data["messages"]

        assert len(messages) == 2
        assert data["has_more"] is True
        assert data["next_cursor"] is not None

        # newest-first order
        t0 = datetime.fromisoformat(messages[0]["created_at"])
        t1 = datetime.fromisoformat(messages[1]["created_at"])
        assert t0 >= t1, f"Expected newest first, but {t0} < {t1}"

        next_cursor = data["next_cursor"]

        # page 2 ──────────────────────────────────────────────────────
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/messages",
            params={"limit": 2, "before": next_cursor},
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        data = body["data"]

        assert len(data["messages"]) == 1
        assert data["has_more"] is False

        for invalid_limit in (0, -1):
            invalid_response = session.get(
                f"{base_url}/api/v1/chat/rooms/{self.room_id}/messages",
                params={"limit": invalid_limit},
                headers=self._auth(self.token_a),
            )
            assert invalid_response.status_code == 400, invalid_response.text
            assert invalid_response.json()["code"] == "VALIDATION_ERROR"

    # ── S5 ────────────────────────────────────────────────────────────

    def test_s5_unauthorized_access(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S5 – Unauthorized access.

        - No token on GET /rooms → 401 AUTHENTICATION_ERROR
        - Invalid token on POST /rooms → 401"""

        # no auth header → GET rooms
        resp = session.get(f"{base_url}/api/v1/chat/rooms")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # invalid token → POST rooms
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": "someone"},
            headers=self._auth("invalid_token"),
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    # ── S6 ────────────────────────────────────────────────────────────

    def test_s6_not_room_member(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S6 – Non-member cannot access the room.

        Register user C, then try to read messages in the A-B
        room.  Must return 403 FORBIDDEN_ERROR."""
        token_c, _user_c = _register_and_login(session, base_url, prefix="chatc")

        # get messages as C
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self.room_id}/messages",
            headers=self._auth(token_c),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

    # ── S7 ────────────────────────────────────────────────────────────

    def test_s7_target_user_not_found(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S7 – Creating a room with a non-existent username → 404."""
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": "nonexistent_user_12345"},
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 404, resp.text
        assert resp.json()["code"] == "NOT_FOUND_ERROR"

    # ── S8 ────────────────────────────────────────────────────────────

    def test_s8_list_rooms(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S8 – Room pages include every room once and reject invalid bounds."""
        create_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "pagination-group",
                "usernames": [self.user_b["username"]],
            },
            headers=self._auth(self.token_a),
        )
        assert create_response.status_code == 201, create_response.text
        group_room_id = create_response.json()["data"]["id"]

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        rooms = body["data"]["rooms"]
        assert isinstance(rooms, list)
        room_ids = [r["id"] for r in rooms]
        assert self.room_id in room_ids
        assert group_room_id in room_ids

        first_page = session.get(
            f"{base_url}/api/v1/chat/rooms",
            params={"limit": 1},
            headers=self._auth(self.token_a),
        )
        second_page = session.get(
            f"{base_url}/api/v1/chat/rooms",
            params={"limit": 1, "offset": 1},
            headers=self._auth(self.token_a),
        )
        assert first_page.status_code == 200, first_page.text
        assert second_page.status_code == 200, second_page.text
        first_data = first_page.json()["data"]
        second_data = second_page.json()["data"]
        assert first_data["has_more"] is True
        assert second_data["has_more"] is False
        assert [page["rooms"][0]["id"] for page in (first_data, second_data)] == room_ids

        for invalid_parameters in ({"limit": 0}, {"limit": 101}, {"offset": -1}):
            invalid_response = session.get(
                f"{base_url}/api/v1/chat/rooms",
                params=invalid_parameters,
                headers=self._auth(self.token_a),
            )
            assert invalid_response.status_code == 400, invalid_response.text
            assert invalid_response.json()["code"] == "VALIDATION_ERROR"

    # ── S9 ────────────────────────────────────────────────────────────

    def test_s9_search_users(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S9 – Search users (auth required).

        Returns a list; each entry has id and username only. Email and
        phone number are private and must not be exposed."""
        resp = session.get(
            f"{base_url}/api/v1/user/search",
            params={"username": self.user_b["username"]},
            headers=self._auth(self.token_a),
        )
        assert resp.status_code == 200, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        users = body["data"]["users"]
        assert isinstance(users, list)
        assert len(users) > 0
        for user in users:
            assert "id" in user
            assert "username" in user
            assert "email" not in user
            assert "phone_number" not in user

        invalid_response = session.get(
            f"{base_url}/api/v1/user/search",
            params={"username": self.user_b["username"], "limit": 0},
            headers=self._auth(self.token_a),
        )
        assert invalid_response.status_code == 400, invalid_response.text
        assert invalid_response.json()["code"] == "VALIDATION_ERROR"

    # ── S10 ───────────────────────────────────────────────────────────

    def test_s10_adjacent_surface_regression(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S10 – Adjacent surface regression.

        Sanity-check that unrelated endpoints still work after chat
        changes."""
        # greet
        resp = session.get(f"{base_url}/greet")
        assert resp.status_code == 200

        # health
        resp = session.get(f"{base_url}/health")
        assert resp.status_code == 200

        # register
        uname = _unique("s10reg")
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
        token, _user = _register_and_login(session, base_url, prefix="s10log")
        assert token is not None

    # ── S11 ────────────────────────────────────────────────────────────

    def test_s11_websocket_fresh_user_empty_rooms(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S11 – Fresh user connects via WS.  `connected.rooms` must be []."""
        token, _user = _register_and_login(session, base_url, prefix="s11")
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, token)
        try:
            msg = _recv(ws)
            assert msg["type"] == "connected"
            assert msg["data"]["rooms"] == []
        finally:
            ws.close()

    # ── S12 ────────────────────────────────────────────────────────────

    def test_s12_websocket_invalid_token(
        self, base_url: str
    ) -> None:
        """S12 – Invalid JWT on WS upgrade → HTTP 401 before upgrade."""
        ws_url = base_url.replace("http", "ws") + "/websocket?token=garbage"
        with pytest.raises(websocket.WebSocketBadStatusException) as exc_info:
            websocket.create_connection(ws_url, timeout=5)
        assert exc_info.value.status_code == 401

    # ── S13 ────────────────────────────────────────────────────────────

    def test_s13_websocket_typing_missing_room_id(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S13 – Typing without room_id → server replies with `error`."""
        token, _user = _register_and_login(session, base_url, prefix="s13")
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, token)
        try:
            _recv(ws)
            ws.send(json.dumps({"type": "typing"}))
            msg = _recv(ws)
            assert msg["type"] == "error"
            assert "room_id" in msg["data"]["message"]
        finally:
            ws.close()

    # ── S14 ────────────────────────────────────────────────────────────

    def test_s14_websocket_missing_type_field(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S14 – WS message without `type` field → server replies with `error`."""
        token, _user = _register_and_login(session, base_url, prefix="s14")
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, token)
        try:
            _recv(ws)
            ws.send(json.dumps({"room_id": "anything"}))
            msg = _recv(ws)
            assert msg["type"] == "error"
            assert "type" in msg["data"]["message"]
        finally:
            ws.close()

    # ── S15 ────────────────────────────────────────────────────────────

    def test_s15_websocket_unknown_message_type(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S15 – Unknown WS message type → server replies with `error`."""
        token, _user = _register_and_login(session, base_url, prefix="s15")
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, token)
        try:
            _recv(ws)
            ws.send(json.dumps({"type": "foobar"}))
            msg = _recv(ws)
            assert msg["type"] == "error"
            assert "foobar" in msg["data"]["message"]
        finally:
            ws.close()

    # ── S16 ────────────────────────────────────────────────────────────

    def test_s16_websocket_pong_backward_compat(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S16 – `pong` is silently ignored (legacy); connection stays alive."""
        token, _user = _register_and_login(session, base_url, prefix="s16")
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, token)
        try:
            _recv(ws)
            ws.send(json.dumps({"type": "pong"}))
            # Connection should still be usable after pong
            ws.send(json.dumps({"type": "foobar"}))
            msg = _recv(ws)
            assert msg["type"] == "error"  # foobar rejected, but not pong
        finally:
            ws.close()

    # ── S17 ────────────────────────────────────────────────────────────

    def test_s17_websocket_rooms_listed_on_connect(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S17 – User with rooms connects → `connected.rooms` lists them."""
        ws_base = base_url.replace("http", "ws")
        ws = _ws_connect(ws_base, self.token_a)
        try:
            msg = _recv(ws)
            assert msg["type"] == "connected"
            assert self.room_id in msg["data"]["rooms"]
        finally:
            ws.close()

    # ── S18 ────────────────────────────────────────────────────────────

    def test_s18_websocket_new_message_broadcast(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S18 – WS send_message → subscriber receives `new_message`."""
        ws_base = base_url.replace("http", "ws")
        ws_b = _ws_connect(ws_base, self.token_b)
        try:
            _recv(ws_b)

            ws_a = _ws_connect(ws_base, self.token_a)
            try:
                _recv(ws_a)

                ws_a.send(
                    json.dumps({
                        "type": "send_message",
                        "data": {
                            "room_id": self.room_id,
                            "content": "hello from S18",
                        },
                    })
                )

                # A receives ack
                _recv_until(ws_a, "message_sent")

                # B receives broadcast
                msg = _recv_until(ws_b, "new_message")
                assert msg["data"]["room_id"] == self.room_id
                assert msg["data"]["sender_id"] == self.user_a["id"]
                assert msg["data"]["content"] == "hello from S18"
            finally:
                ws_a.close()
        finally:
            ws_b.close()

    # ── S19 ────────────────────────────────────────────────────────────

    def test_s19_websocket_typing_indicator(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S19 – User sends `typing` → other room member receives indicator."""
        ws_base = base_url.replace("http", "ws")

        ws_b = _ws_connect(ws_base, self.token_b)
        try:
            _recv(ws_b)

            ws_a = _ws_connect(ws_base, self.token_a)
            try:
                _recv(ws_a)

                ws_a.send(
                    json.dumps({"type": "typing", "room_id": self.room_id})
                )

                msg = _recv_until(ws_b, "typing")
                assert msg["data"]["room_id"] == self.room_id
                assert msg["data"]["user_id"] == self.user_a["id"]
                assert msg["data"]["username"] == self.user_a["username"]
                assert msg["data"]["typing"] is True
            finally:
                ws_a.close()
        finally:
            ws_b.close()

    # ── S20 ────────────────────────────────────────────────────────────

    def test_s20_websocket_user_online_offline(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S20 – A connects → B sees `user_online`; A disconnects → B sees `user_offline`."""
        ws_base = base_url.replace("http", "ws")

        ws_b = _ws_connect(ws_base, self.token_b)
        try:
            _recv(ws_b)

            ws_a = _ws_connect(ws_base, self.token_a)
            try:
                _recv(ws_a)

                msg = _recv_until(ws_b, "user_online")
                assert msg["data"]["user_id"] == self.user_a["id"]
                assert msg["data"]["username"] == self.user_a["username"]
            finally:
                ws_a.close()

            msg = _recv_until(ws_b, "user_offline")
            assert msg["data"]["user_id"] == self.user_a["id"]
            assert msg["data"]["username"] == self.user_a["username"]
        finally:
            ws_b.close()

    # ── S21 ────────────────────────────────────────────────────────────

    def test_s21_websocket_leave_stops_messages(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S21 – User leaves room → WS stops receiving new_message for that room.

        Flow:
          1. B connects WS (auto-subscribed to A-B room).
          2. A connects WS.
          3. A sends msg → B receives `new_message` (subscription active).
          4. B leaves the room via HTTP DELETE.
          5. A sends another msg → B must NOT receive it (cancel done).
        """
        ws_base = base_url.replace("http", "ws")
        ws_b = _ws_connect(ws_base, self.token_b)
        try:
            _recv(ws_b)

            ws_a = _ws_connect(ws_base, self.token_a)
            try:
                _recv(ws_a)

                # Confirm subscription works before leave
                ws_a.send(
                    json.dumps({
                        "type": "send_message",
                        "data": {
                            "room_id": self.room_id,
                            "content": "before leave",
                        },
                    })
                )
                _recv_until(ws_a, "message_sent")  # consume ack
                _recv_until(ws_b, "new_message")  # consume broadcast

                # B leaves the room
                resp = session.delete(
                    f"{base_url}/api/v1/chat/rooms/"
                    f"{self.room_id}/members/{self.user_b['id']}",
                    headers=self._auth(self.token_b),
                )
                assert resp.status_code == 200, resp.text

                # A sends another message
                ws_a.send(
                    json.dumps({
                        "type": "send_message",
                        "data": {
                            "room_id": self.room_id,
                            "content": "after leave",
                        },
                    })
                )
                _recv_until(ws_a, "message_sent")  # consume ack

                # B should NOT receive this message
                ws_b.settimeout(3)
                try:
                    while True:
                        raw = ws_b.recv()
                        m = json.loads(raw)
                        if m["type"] == "new_message":
                            pytest.fail(
                                f"Received new_message after leave: {m}"
                            )
                except websocket.WebSocketTimeoutException:
                    pass  # expected — no message arrived
            finally:
                ws_a.close()
        finally:
            ws_b.close()

    # ── S22 ────────────────────────────────────────────────────────────

    def test_s22_typing_not_sent_to_sender(
        self, session: requests.Session, base_url: str
    ) -> None:
        """S22 – Typing indicator is not echoed back to the sender.

        Flow:
          1. A and B both connect WS to a fresh shared room.
          2. A sends a typing event.
          3. B receives the typing indicator.
          4. A must NOT receive their own typing indicator.

        Note: uses a fresh room because S21 removed user_b from
        self.room_id, so that room is no longer shared.
        """
        # Register fresh users and create a new room for this test,
        # so we don't depend on state from earlier tests (S21 removes user_b).
        token_a, user_a = _register_and_login(session, base_url, prefix="s22a")
        token_b, user_b = _register_and_login(session, base_url, prefix="s22b")

        # A sends B a private room request
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests",
            json={
                "receiver_id": user_b["id"],
                "message": "hello",
                "is_encrypted": False,
            },
            headers=self._auth(token_a),
        )
        assert resp.status_code == 201, resp.text
        request_id = resp.json()["data"]["request_id"]

        # B accepts the request; take the room id from the accept response
        accept_resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=self._auth(token_b),
        )
        assert accept_resp.status_code == 200, accept_resp.text
        room_id = accept_resp.json()["data"]["room"]["id"]

        ws_base = base_url.replace("http", "ws")

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)  # consume "connected"

            ws_b = _ws_connect(ws_base, token_b)
            try:
                _recv(ws_b)  # consume "connected"

                # Send typing from A
                ws_a.send(
                    json.dumps({"type": "typing", "room_id": room_id})
                )

                # B must receive the typing indicator
                msg = _recv_until(ws_b, "typing", timeout=5)
                assert msg["data"]["user_id"] == user_a["id"]
                assert msg["data"]["username"] == user_a["username"]
                assert msg["data"]["typing"] is True

                # A must NOT receive the typing indicator
                ws_a.settimeout(3)
                try:
                    while True:
                        raw = ws_a.recv()
                        m = json.loads(raw)
                        if m["type"] == "typing":
                            pytest.fail(
                                f"A received their own typing indicator: {m}"
                            )
                except websocket.WebSocketTimeoutException:
                    pass  # expected — no typing broadcast to sender
            finally:
                ws_b.close()
        finally:
            ws_a.close()


# ── account deletion preserves chat history ─────────────────────────

class TestDeletedAccountPreservesChat:
    """Deleting a user must keep rooms and messages; the sender and
    creator references become NULL so clients can render a deactivated
    user instead of losing the conversation."""

    @staticmethod
    def _auth(token: str) -> dict:
        return {"Authorization": f"Bearer {token}"}

    def test_rooms_and_messages_survive_account_deletion(
        self, session: requests.Session, base_url: str
    ) -> None:
        """A and B chat in a private room, A deletes the account.

        B must still see the room and every message, with A's
        references nulled out (sender_id / created_by)."""
        token_a, user_a = _register_and_login(session, base_url, prefix="delchata")
        token_b, user_b = _register_and_login(session, base_url, prefix="delchatb")

        # A creates a group room containing A and B (no room request needed).
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={
                "is_group": True,
                "name": "del-test-group",
                "usernames": [user_b["username"]],
            },
            headers=self._auth(token_a),
        )
        assert resp.status_code == 201, resp.text
        room_id = resp.json()["data"]["id"]

        # A sends a message over WebSocket.
        ws_base = base_url.replace("http", "ws")
        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)  # consume "connected"
            ws_a.send(
                json.dumps({
                    "type": "send_message",
                    "data": {"room_id": room_id, "content": "bye from A"},
                })
            )
            ack = _recv_until(ws_a, "message_sent")
            assert ack["data"]["sender_id"] == user_a["id"]
        finally:
            ws_a.close()

        # A deletes the account (password verified).
        del_resp = session.delete(
            f"{base_url}/api/v1/user/me",
            json={"password": "P@ssw0rd!"},
            headers=self._auth(token_a),
        )
        assert del_resp.status_code == 200, del_resp.text

        # B still reads the room message history; A's sender_id is null.
        msgs = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}/messages",
            headers=self._auth(token_b),
        )
        assert msgs.status_code == 200, msgs.text
        messages = msgs.json()["data"]["messages"]
        assert len(messages) == 1, messages
        assert messages[0]["content"] == "bye from A"
        assert messages[0]["sender_id"] is None

        # B still sees the room detail; A's creator reference is null.
        detail = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}",
            headers=self._auth(token_b),
        )
        assert detail.status_code == 200, detail.text
        assert detail.json()["data"]["id"] == room_id
        assert detail.json()["data"]["created_by"] is None

        # The room still appears in B's room list and the last-message
        # preview survives with an empty sender username.
        rooms = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=self._auth(token_b),
        )
        assert rooms.status_code == 200, rooms.text
        listed = [r for r in rooms.json()["data"]["rooms"] if r["id"] == room_id]
        assert len(listed) == 1, rooms.text
        assert listed[0]["last_message"]["content"] == "bye from A"
        assert listed[0]["last_message"]["sender_username"] is None
        assert listed[0]["created_by"] is None
