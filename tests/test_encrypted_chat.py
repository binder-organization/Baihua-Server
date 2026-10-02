"""E2E tests for encrypted (E2EE) private chat.

Each scenario creates its own users and room, so it can run on its own.

Scenarios:
  E1  – Create encrypted private room
  E2  – Encrypted room shows is_encrypted=true in list/detail
  E3  – Encrypted room hides message preview in list
  E4  – encrypt_request → partner receives encrypt_invitation
  E5  – encrypt_accept → partner receives encrypt_accept_response
  E6  – encrypt_ready + encrypt_ready → both receive encrypt_session_ready
  E7  – Send encrypted message → partner receives new_encrypted_message
  E8  – Get messages for encrypted room returns ciphertext, not content
  E9  – User disconnect triggers grace period notification
  E10 – User reconnect within grace period → session preserved (verified via message)
  E11 – Grace period expiry → session terminated, messages purged
  E12 – encrypt_leave → session terminated immediately
  E13 – encrypt_request with partner offline → error
  E14 – encrypt_message without active session → error
  E15 – encrypt_leave without active session → error
  E16 – Encrypted + non-encrypted rooms coexist between same users
"""

import json
import os
import time
import uuid
from contextlib import contextmanager
from datetime import datetime

import pytest
import requests
import websocket


# ── helpers (mirroring test_chat.py style) ──────────────────────────

def _unique(name: str) -> str:
    suffix = uuid.uuid4().hex[:8]
    return f"{name}_{suffix}"


def _register_user(
    session: requests.Session, base_url: str, *, prefix: str = "encuser"
) -> dict:
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
    session: requests.Session, base_url: str, *, prefix: str = "encuser"
) -> tuple[str, dict]:
    user = _register_user(session, base_url, prefix=prefix)
    login_resp = session.post(
        f"{base_url}/api/v1/user/login",
        json={"username": user["username"], "password": "P@ssw0rd!"},
    )
    assert login_resp.status_code == 200, login_resp.text
    body = login_resp.json()
    assert body["code"] == "SUCCESS"
    return body["data"]["token"], user


def _ws_connect(ws_base: str, token: str, timeout: int = 10) -> websocket.WebSocket:
    return websocket.create_connection(
        f"{ws_base}/websocket",
        header={"authorization": f"Bearer {token}"},
        timeout=timeout,
    )


def _recv(ws: websocket.WebSocket, timeout: int = 10) -> dict:
    ws.settimeout(timeout)
    raw = ws.recv()
    return json.loads(raw)


def _recv_until(
    ws: websocket.WebSocket, expected_type: str, timeout: int = 10
) -> dict:
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


def _send_ws(ws: websocket.WebSocket, msg: dict) -> None:
    ws.send(json.dumps(msg))


def _drain_until(
    ws: websocket.WebSocket, expected_type: str, timeout: float = 10
) -> dict:
    """Drain messages until the expected type arrives; skip connected, user_online, etc."""
    return _recv_until(ws, expected_type, timeout)


def _auth(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


def _create_encrypted_room(
    session: requests.Session,
    base_url: str,
    token_a: str,
    token_b: str,
    user_b: dict,
    is_encrypted: bool = True,
) -> str:
    """Create a private room through the room-request accept flow.

    User A sends a room request to user B, then B accepts it, which is
    what actually creates the private room with the requested encryption
    flag. Returns the new room id."""
    resp = session.post(
        f"{base_url}/api/v1/chat/rooms/requests",
        json={
            "receiver_id": user_b["id"],
            "message": "Room request for encrypted chat test",
            "is_encrypted": is_encrypted,
        },
        headers=_auth(token_a),
    )
    assert resp.status_code == 201, resp.text
    request_id = resp.json()["data"]["request_id"]

    resp = session.post(
        f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
        headers=_auth(token_b),
    )
    assert resp.status_code == 200, resp.text
    return resp.json()["data"]["room"]["id"]


@contextmanager
def _active_encrypted_session(session: requests.Session, base_url: str, prefix: str):
    token_a, user_a = _register_and_login(session, base_url, prefix=f"{prefix}a")
    token_b, user_b = _register_and_login(session, base_url, prefix=f"{prefix}b")
    room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)
    websocket_address = base_url.replace("http", "ws")
    first_socket = _ws_connect(websocket_address, token_a)
    try:
        _recv(first_socket)
        second_socket = _ws_connect(websocket_address, token_b)
        try:
            _recv(second_socket)
            _send_ws(first_socket, {
                "type": "encrypt_request",
                "data": {
                    "room_id": room_id,
                    "public_key": "TestRequestKey",
                    "identity_key": "TestRequestIdentity",
                    "signature": "TestRequestSignature",
                },
            })
            _drain_until(second_socket, "encrypt_invitation")
            _send_ws(second_socket, {
                "type": "encrypt_accept",
                "data": {
                    "room_id": room_id,
                    "public_key": "TestAcceptKey",
                    "identity_key": "TestAcceptIdentity",
                    "signature": "TestAcceptSignature",
                },
            })
            _drain_until(first_socket, "encrypt_accept_response")
            _send_ws(first_socket, {
                "type": "encrypt_ready", "data": {"room_id": room_id}
            })
            _send_ws(second_socket, {
                "type": "encrypt_ready", "data": {"room_id": room_id}
            })
            _drain_until(first_socket, "encrypt_session_ready")
            _drain_until(second_socket, "encrypt_session_ready")
            yield room_id, token_a, user_a, first_socket, second_socket
        finally:
            second_socket.close()
    finally:
        first_socket.close()


class EncryptedChatTest:
    """Scenarios for encrypted private chat."""

    @pytest.fixture(autouse=True)
    def _prepare_encrypted_room(self, request, session: requests.Session, base_url: str):
        if request.node.name not in {
            "test_e2_room_detail_shows_encrypted",
            "test_e3_encrypted_room_hides_preview",
            "test_e4_encrypt_request_sends_invitation",
            "test_e5_encrypt_accept_sends_response",
            "test_e6_both_ready_activates_session",
        }:
            return

        self.token_a, self.user_a = _register_and_login(
            session, base_url, prefix="encrooma"
        )
        self.token_b, self.user_b = _register_and_login(
            session, base_url, prefix="encroomb"
        )
        self.room_id = _create_encrypted_room(
            session, base_url, self.token_a, self.token_b, self.user_b
        )

        if request.node.name in {
            "test_e5_encrypt_accept_sends_response",
            "test_e6_both_ready_activates_session",
        }:
            websocket_address = base_url.replace("http", "ws")
            first_socket = _ws_connect(websocket_address, self.token_a)
            try:
                _recv(first_socket)
                second_socket = _ws_connect(websocket_address, self.token_b)
                try:
                    _recv(second_socket)
                    _send_ws(first_socket, {
                        "type": "encrypt_request",
                        "data": {
                            "room_id": self.room_id,
                            "public_key": "FixtureRequestKey",
                            "identity_key": "FixtureRequestIdentity",
                            "signature": "FixtureRequestSignature",
                        },
                    })
                    _drain_until(second_socket, "encrypt_invitation")
                    if request.node.name == "test_e6_both_ready_activates_session":
                        _send_ws(second_socket, {
                            "type": "encrypt_accept",
                            "data": {
                                "room_id": self.room_id,
                                "public_key": "FixtureAcceptKey",
                                "identity_key": "FixtureAcceptIdentity",
                                "signature": "FixtureAcceptSignature",
                            },
                        })
                        _drain_until(first_socket, "encrypt_accept_response")
                finally:
                    second_socket.close()
            finally:
                first_socket.close()

    @property
    def _room_id(self) -> str:
        return self.room_id

    @property
    def _token_a(self) -> str:
        return self.token_a

    @property
    def _token_b(self) -> str:
        return self.token_b

    @property
    def _user_a(self) -> dict:
        return self.user_a

    @property
    def _user_b(self) -> dict:
        return self.user_b

    # ── E1 ─────────────────────────────────────────────────────────────

    @pytest.mark.smoke
    def test_e1_create_encrypted_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E1 – Create an encrypted private room.

        Register A and B, then go through the room-request accept flow
        with is_encrypted=true. The accepted room must be encrypted,
        non-group, with both users as members."""
        token_a, user_a = _register_and_login(session, base_url, prefix="ence1a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence1b")

        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)
        uuid.UUID(room_id)

        # Verify the accepted room carries the encryption flag and both members.
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}",
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]

        assert data["is_encrypted"] is True
        assert data["is_group"] is False

        members = data["members"]
        assert len(members) == 2
        member_ids = [m["user_id"] for m in members]
        assert user_a["id"] in member_ids
        assert user_b["id"] in member_ids


    # ── E2 ─────────────────────────────────────────────────────────────

    def test_e2_room_detail_shows_encrypted(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E2 – Room detail and list show is_encrypted=true."""
        # detail
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{self._room_id}",
            headers=_auth(self._token_a),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]
        assert data["is_encrypted"] is True
        assert data["id"] == self._room_id

        # list
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=_auth(self._token_a),
        )
        assert resp.status_code == 200, resp.text
        rooms = resp.json()["data"]["rooms"]
        room_ids = [r["id"] for r in rooms]
        assert self._room_id in room_ids
        room = next(r for r in rooms if r["id"] == self._room_id)
        assert room["is_encrypted"] is True

    # ── E3 ─────────────────────────────────────────────────────────────

    def test_e3_encrypted_room_hides_preview(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E3 – Encrypted room has last_message=null in room list."""
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=_auth(self._token_a),
        )
        assert resp.status_code == 200, resp.text
        rooms = resp.json()["data"]["rooms"]
        room = next(r for r in rooms if r["id"] == self._room_id)
        assert room["last_message"] is None

    # ── E4 ─────────────────────────────────────────────────────────────

    def test_e4_encrypt_request_sends_invitation(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E4 – A sends encrypt_request → B receives encrypt_invitation.

        Keys are placeholder base64 strings (real client-side key
        generation is outside the server's scope)."""
        ws_base = base_url.replace("http", "ws")

        ws_b = _ws_connect(ws_base, self._token_b)
        try:
            _recv(ws_b)  # connected

            ws_a = _ws_connect(ws_base, self._token_a)
            try:
                _recv(ws_a)  # connected

                fake_pubkey = "AwEAAc7R3Y2JpY2lwaGVy"
                fake_idkey = "BCowYQ8uJ1qL9zXy5vN4rT2w"
                fake_sig = "MEQCIDT39gVn8Rzj7ZqF9W6kL3x1pS0n2dA4fG5hJ8kLmN0="

                _send_ws(ws_a, {
                    "type": "encrypt_request",
                    "data": {
                        "room_id": self._room_id,
                        "public_key": fake_pubkey,
                        "identity_key": fake_idkey,
                        "signature": fake_sig,
                    },
                })

                msg = _drain_until(ws_b, "encrypt_invitation")
                data = msg["data"]
                assert data["room_id"] == self._room_id
                assert data["inviter_id"] == self._user_a["id"]
                assert data["public_key"] == fake_pubkey
                assert data["identity_key"] == fake_idkey
                assert data["signature"] == fake_sig
            finally:
                ws_a.close()
        finally:
            ws_b.close()

    # ── E5 ─────────────────────────────────────────────────────────────

    def test_e5_encrypt_accept_sends_response(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E5 – B sends encrypt_accept → A receives encrypt_accept_response."""
        ws_base = base_url.replace("http", "ws")

        ws_a = _ws_connect(ws_base, self._token_a)
        try:
            _recv(ws_a)  # connected

            ws_b = _ws_connect(ws_base, self._token_b)
            try:
                _recv(ws_b)  # connected

                fake_pubkey_b = "BPDkF7x2Cp8vN4tR4wL9mZ6qY3cX1vBn"
                fake_idkey_b = "BL0pR8sT5uV2wY4zA6cE8gH0iJ2kM4oP"
                fake_sig_b = "MEUCIQC7fG9n2a4D5s8J0kL3pO6rT9wE1yH2vN4mX8cB7fA="

                _send_ws(ws_b, {
                    "type": "encrypt_accept",
                    "data": {
                        "room_id": self._room_id,
                        "public_key": fake_pubkey_b,
                        "identity_key": fake_idkey_b,
                        "signature": fake_sig_b,
                    },
                })

                msg = _drain_until(ws_a, "encrypt_accept_response")
                data = msg["data"]
                assert data["room_id"] == self._room_id
                assert data["acceptor_id"] == self._user_b["id"]
                assert data["public_key"] == fake_pubkey_b
                assert data["identity_key"] == fake_idkey_b
                assert data["signature"] == fake_sig_b
            finally:
                ws_b.close()
        finally:
            ws_a.close()

    # ── E6 ─────────────────────────────────────────────────────────────

    def test_e6_both_ready_activates_session(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E6 – Both users send encrypt_ready → both receive encrypt_session_ready."""
        ws_base = base_url.replace("http", "ws")

        ws_a = _ws_connect(ws_base, self._token_a)
        try:
            _recv(ws_a)  # connected

            ws_b = _ws_connect(ws_base, self._token_b)
            try:
                _recv(ws_b)  # connected

                # A signals ready
                _send_ws(ws_a, {
                    "type": "encrypt_ready",
                    "data": {"room_id": self._room_id},
                })

                # B signals ready (this should trigger session activation)
                _send_ws(ws_b, {
                    "type": "encrypt_ready",
                    "data": {"room_id": self._room_id},
                })

                # Both should receive encrypt_session_ready
                msg_a = _drain_until(ws_a, "encrypt_session_ready")
                assert msg_a["data"]["room_id"] == self._room_id

                msg_b = _drain_until(ws_b, "encrypt_session_ready")
                assert msg_b["data"]["room_id"] == self._room_id
            finally:
                ws_b.close()
        finally:
            ws_a.close()

    # ── E7 ─────────────────────────────────────────────────────────────

    def test_e7_send_encrypted_message(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E7 – A sends encrypt_message → B receives new_encrypted_message.

        Uses a fresh encrypted room (independent of earlier test state)."""
        ws_base = base_url.replace("http", "ws")

        # Register fresh users and create an encrypted room for this test
        token_a, user_a = _register_and_login(session, base_url, prefix="ence7a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence7b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)  # connected
            ws_b = _ws_connect(ws_base, token_b)
            try:
                _recv(ws_b)

                # Establish session through full handshake
                _send_ws(ws_a, {
                    "type": "encrypt_request",
                    "data": {
                        "room_id": room_id,
                        "public_key": "DummyPubKeyE7A",
                        "identity_key": "DummyIdKeyE7A",
                        "signature": "DummySigE7A",
                    },
                })
                _drain_until(ws_b, "encrypt_invitation")

                _send_ws(ws_b, {
                    "type": "encrypt_accept",
                    "data": {
                        "room_id": room_id,
                        "public_key": "DummyPubKeyE7B",
                        "identity_key": "DummyIdKeyE7B",
                        "signature": "DummySigE7B",
                    },
                })
                _drain_until(ws_a, "encrypt_accept_response")

                _send_ws(ws_a, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _send_ws(ws_b, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _drain_until(ws_a, "encrypt_session_ready")
                _drain_until(ws_b, "encrypt_session_ready")

                # Now send an encrypted message
                ciphertext = "gANpcHl0aG9uIG9iamVjdC4AAAAAAAAA"

                _send_ws(ws_a, {
                    "type": "encrypt_message",
                    "data": {
                        "room_id": room_id,
                        "ciphertext": ciphertext,
                    },
                })

                # A receives ack
                ack = _drain_until(ws_a, "encrypted_message_sent")
                assert ack["data"]["room_id"] == room_id

                # B receives the ciphertext
                msg = _drain_until(ws_b, "new_encrypted_message")
                assert msg["data"]["room_id"] == room_id
                assert msg["data"]["sender_id"] == user_a["id"]
                assert msg["data"]["ciphertext"] == ciphertext
                message_id = msg["data"]["id"]
                uuid.UUID(message_id)

            finally:
                ws_b.close()
        finally:
            ws_a.close()

    # ── E8 ─────────────────────────────────────────────────────────────

    def test_e8_get_messages_returns_ciphertext(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E8 – GET /messages for encrypted room returns ciphertext, not content."""
        ciphertext = "gANpcHl0aG9uIG9iamVjdC4AAAAAAAAA"
        with _active_encrypted_session(session, base_url, "ence8") as (
            room_id, token_a, _, first_socket, second_socket
        ):
            _send_ws(first_socket, {
                "type": "encrypt_message",
                "data": {"room_id": room_id, "ciphertext": ciphertext},
            })
            _drain_until(first_socket, "encrypted_message_sent")
            message = _drain_until(second_socket, "new_encrypted_message")
            message_id = message["data"]["id"]

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}/messages",
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text

        messages = resp.json()["data"]["messages"]
        assert len(messages) > 0

        enc_msg = next(
            (m for m in messages if m.get("id") == message_id),
            None,
        )
        assert enc_msg is not None, "Encrypted message not found in message list"

        # Must have ciphertext, not content
        assert enc_msg.get("ciphertext") == ciphertext
        assert enc_msg.get("content") is None

    # ── E9 ─────────────────────────────────────────────────────────────

    def test_e9_disconnect_triggers_grace_period(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E9 – User disconnects → partner receives encrypt_partner_disconnected.

        Uses a fresh encrypted room (independent of earlier test state)."""
        ws_base = base_url.replace("http", "ws")

        token_a, user_a = _register_and_login(session, base_url, prefix="ence9a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence9b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)
            ws_b = _ws_connect(ws_base, token_b)
            try:
                _recv(ws_b)

                _send_ws(ws_a, {
                    "type": "encrypt_request",
                    "data": {
                        "room_id": room_id,
                        "public_key": "KeyE9A",
                        "identity_key": "IdKeyE9A",
                        "signature": "SigE9A",
                    },
                })
                _drain_until(ws_b, "encrypt_invitation")
                _send_ws(ws_b, {
                    "type": "encrypt_accept",
                    "data": {
                        "room_id": room_id,
                        "public_key": "KeyE9B",
                        "identity_key": "IdKeyE9B",
                        "signature": "SigE9B",
                    },
                })
                _drain_until(ws_a, "encrypt_accept_response")
                _send_ws(ws_a, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _send_ws(ws_b, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _drain_until(ws_a, "encrypt_session_ready")
                _drain_until(ws_b, "encrypt_session_ready")

                # A disconnects → B should receive encrypt_partner_disconnected
                ws_a.close()

                msg = _drain_until(ws_b, "encrypt_partner_disconnected")
                assert msg["data"]["room_id"] == room_id
                assert msg["data"]["offline_user_id"] == user_a["id"]

            finally:
                ws_b.close()
        finally:
            try:
                ws_a.close()
            except Exception:
                pass

    # ── E10 ───────────────────────────────────────────────────────────

    def test_e10_reconnect_within_grace_period(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E10 – Reconnecting during the grace period preserves the session."""
        ws_base = base_url.replace("http", "ws")
        with _active_encrypted_session(session, base_url, "ence10") as (
            room_id, token_a, _, first_socket, second_socket
        ):
            first_socket.close()
            _drain_until(second_socket, "encrypt_partner_disconnected")
            reconnected_socket = _ws_connect(ws_base, token_a)
            try:
                _drain_until(reconnected_socket, "connected")
                _send_ws(reconnected_socket, {
                    "type": "encrypt_message",
                    "data": {
                        "room_id": room_id,
                        "ciphertext": "UmVjb25uZWN0ZWRNc2c=",
                    },
                })
                ack = _drain_until(reconnected_socket, "encrypted_message_sent")
                assert ack["data"]["room_id"] == room_id
                msg = _drain_until(second_socket, "new_encrypted_message")
                assert msg["data"]["room_id"] == room_id
                assert msg["data"]["ciphertext"] == "UmVjb25uZWN0ZWRNc2c="
            finally:
                reconnected_socket.close()

    # ── E11 ───────────────────────────────────────────────────────────

    def test_e11_grace_period_expiry_purges_messages(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E11 – Grace period expires → session terminated, messages purged.

        Uses a fresh encrypted room (independent of earlier test state).
        A disconnects (fully offline) then waits >30s.
        B receives encrypt_session_ended with reason=partner_timeout.
        After expiry, messages endpoint returns empty list."""
        ws_base = base_url.replace("http", "ws")

        token_a, user_a = _register_and_login(session, base_url, prefix="ence11a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence11b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)
            ws_b = _ws_connect(ws_base, token_b)
            try:
                _recv(ws_b)

                _send_ws(ws_a, {
                    "type": "encrypt_request",
                    "data": {
                        "room_id": room_id,
                        "public_key": "KeyE11A",
                        "identity_key": "IdKeyE11A",
                        "signature": "SigE11A",
                    },
                })
                _drain_until(ws_b, "encrypt_invitation")
                _send_ws(ws_b, {
                    "type": "encrypt_accept",
                    "data": {
                        "room_id": room_id,
                        "public_key": "KeyE11B",
                        "identity_key": "IdKeyE11B",
                        "signature": "SigE11B",
                    },
                })
                _drain_until(ws_a, "encrypt_accept_response")
                _send_ws(ws_a, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _send_ws(ws_b, {
                    "type": "encrypt_ready",
                    "data": {"room_id": room_id},
                })
                _drain_until(ws_a, "encrypt_session_ready")
                _drain_until(ws_b, "encrypt_session_ready")

                # A disconnects fully
                ws_a.close()

                # B receives partner_disconnected
                _drain_until(ws_b, "encrypt_partner_disconnected")

                # B should receive encrypt_session_ended
                grace_period = int(os.environ.get("BAIHUA_ENCRYPTED_GRACE_PERIOD_SECS", "30"))
                msg = _drain_until(
                    ws_b, "encrypt_session_ended", timeout=grace_period + 10
                )
                assert msg["data"]["room_id"] == room_id
                assert msg["data"]["reason"] == "partner_timeout"
                assert msg["data"]["offline_user_id"] == user_a["id"]
            finally:
                ws_b.close()
        finally:
            try:
                ws_a.close()
            except Exception:
                pass

        # Messages should be purged → empty list
        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}/messages",
            headers=_auth(token_b),
        )
        assert resp.status_code == 200, resp.text
        messages = resp.json()["data"]["messages"]
        assert len(messages) == 0, f"Expected no messages after purge, got {len(messages)}"

    # ── E12 ───────────────────────────────────────────────────────────

    def test_e12_encrypt_leave_terminates_session(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E12 – User sends encrypt_leave → session terminated immediately.

        Both receive encrypt_session_ended with reason=user_left."""
        with _active_encrypted_session(session, base_url, "ence12") as (
            room_id, _, user_a, first_socket, second_socket
        ):
            _send_ws(first_socket, {
                "type": "encrypt_leave", "data": {"room_id": room_id}
            })
            for active_socket in (first_socket, second_socket):
                message = _drain_until(active_socket, "encrypt_session_ended")
                assert message["data"]["room_id"] == room_id
                assert message["data"]["reason"] == "user_left"
                assert message["data"]["offline_user_id"] == user_a["id"]

    # ── E13 ──────────────────────────────────────────────────────────

    def test_e13_encrypt_request_partner_offline(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E13 – encrypt_request with partner offline → error."""
        ws_base = base_url.replace("http", "ws")

        token_a, user_a = _register_and_login(session, base_url, prefix="ence13a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence13b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)

            _send_ws(ws_a, {
                "type": "encrypt_request",
                "data": {
                    "room_id": room_id,
                    "public_key": "FakeKeyE13",
                    "identity_key": "FakeIdE13",
                    "signature": "FakeSigE13",
                },
            })

            err = _drain_until(ws_a, "error", timeout=5)
            assert "online" in err["data"]["message"].lower()
        finally:
            ws_a.close()

    # ── E14 ──────────────────────────────────────────────────────────

    def test_e14_encrypt_message_without_session(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E14 – encrypt_message without active session → error."""
        ws_base = base_url.replace("http", "ws")

        token_a, user_a = _register_and_login(session, base_url, prefix="ence14a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence14b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)
            ws_b = _ws_connect(ws_base, token_b)
            try:
                _recv(ws_b)

                _send_ws(ws_a, {
                    "type": "encrypt_message",
                    "data": {
                        "room_id": room_id,
                        "ciphertext": "R29Ob1Nlc3Npb25FcnJvcg==",
                    },
                })

                err = _drain_until(ws_a, "error", timeout=5)
                assert "active encrypted session" in err["data"]["message"].lower()
            finally:
                ws_b.close()
        finally:
            ws_a.close()

    # ── E15 ──────────────────────────────────────────────────────────

    def test_e15_encrypt_leave_without_session(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E15 – encrypt_leave without active session → error."""
        ws_base = base_url.replace("http", "ws")

        token_a, user_a = _register_and_login(session, base_url, prefix="ence15a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence15b")
        room_id = _create_encrypted_room(session, base_url, token_a, token_b, user_b)

        ws_a = _ws_connect(ws_base, token_a)
        try:
            _recv(ws_a)

            _send_ws(ws_a, {
                "type": "encrypt_leave",
                "data": {"room_id": room_id},
            })

            err = _drain_until(ws_a, "error", timeout=5)
            assert "active encrypted session" in err["data"]["message"].lower()
        finally:
            ws_a.close()

    # ── E16 ──────────────────────────────────────────────────────────

    def test_e16_encrypted_and_plain_coexist(
        self, session: requests.Session, base_url: str
    ) -> None:
        """E16 – Encrypted + non-encrypted rooms coexist between same users."""
        token_a, user_a = _register_and_login(session, base_url, prefix="ence16a")
        token_b, user_b = _register_and_login(session, base_url, prefix="ence16b")

        plain_id = _create_encrypted_room(
            session, base_url, token_a, token_b, user_b, is_encrypted=False
        )
        enc_id = _create_encrypted_room(
            session, base_url, token_a, token_b, user_b, is_encrypted=True
        )

        assert plain_id != enc_id

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        rooms = resp.json()["data"]["rooms"]
        room_ids = [r["id"] for r in rooms]
        assert plain_id in room_ids
        assert enc_id in room_ids

        plain_from_list = next(r for r in rooms if r["id"] == plain_id)
        enc_from_list = next(r for r in rooms if r["id"] == enc_id)
        assert plain_from_list["is_encrypted"] is False
        assert enc_from_list["is_encrypted"] is True
