"""End-to-end tests for the room request (pre-negotiation) flow.

Covers the endpoints:
  - GET  /api/v1/user/search
  - POST /api/v1/chat/rooms/requests
  - GET  /api/v1/chat/rooms/requests/pending
  - GET  /api/v1/chat/rooms/requests/sent
  - POST /api/v1/chat/rooms/requests/{request_id}/accept
  - POST /api/v1/chat/rooms/requests/{request_id}/decline
  - POST /api/v1/chat/rooms/requests/{request_id}/cancel
  - POST /api/v1/chat/rooms (private-room gate)

The server envelope is {response_id, code, message, data}; success means
code == "SUCCESS". Errors use codes such as VALIDATION_ERROR,
BAD_REQUEST_ERROR, CONFLICT_ERROR, FORBIDDEN_ERROR, NOT_FOUND_ERROR and
AUTHENTICATION_ERROR.

Every test registers its own fresh users, so the tests are independent
of each other (requests are scoped per sender/receiver pair).

# Manual verification: the sender daily-limit scenario is NOT automated
# here, because the limit is read from the `[room_request]` section of
# `~/.baihua/config.toml` when the server starts. To verify it by hand:
# stop the server, set `send_daily_limit = 3` in `~/.baihua/config.toml`,
# start the server, and confirm that the fourth send attempt returns 429
# RATE_LIMIT_ERROR. Restore the default and restart afterwards.
# The receiver inbox-cap overflow returns 409 CONFLICT_ERROR (not 429)
# because it is a state conflict, not a rate limit.
"""

import os
import uuid

import psycopg2
import pytest
import requests


# ── helpers (mirroring test_chat.py style) ────────────────────────────

def _unique(name: str) -> str:
    """Generate a unique username/email to avoid cross-run conflicts."""
    suffix = uuid.uuid4().hex[:8]
    return f"{name}_{suffix}"


def _auth(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


def _register_user(
    session: requests.Session, base_url: str, *, prefix: str = "roomreq"
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
    session: requests.Session, base_url: str, *, prefix: str = "roomreq"
) -> tuple[str, dict]:
    """Register + login a unique user. Returns (token, user_data)."""
    user = _register_user(session, base_url, prefix=prefix)
    login_resp = session.post(
        f"{base_url}/api/v1/user/login",
        json={"username": user["username"], "password": "P@ssw0rd!"},
    )
    assert login_resp.status_code == 200, login_resp.text
    body = login_resp.json()
    assert body["code"] == "SUCCESS"
    return body["data"]["token"], user


def _send_request(
    session: requests.Session,
    base_url: str,
    token: str,
    receiver_id: str,
    *,
    message: str = "hello, let me open a room with you",
    is_encrypted: bool = False,
) -> requests.Response:
    """Send a room request from the authenticated user to receiver_id."""
    return session.post(
        f"{base_url}/api/v1/chat/rooms/requests",
        json={
            "receiver_id": receiver_id,
            "message": message,
            "is_encrypted": is_encrypted,
        },
        headers=_auth(token),
    )


def _expire_request(request_id: str) -> None:
    """Backdate a room request's created_at beyond the expiry window.

    The default expiry_hours config is 120, so shifting created_at by
    more than that makes the next sweep mark the row as expired. This
    keeps the lazy-sweep behaviour testable without waiting days.
    """
    conn = psycopg2.connect(
        host=os.environ.get("POSTGRES_HOST", "localhost"),
        port=int(os.environ.get("POSTGRES_PORT", "2423")),
        dbname=os.environ.get("POSTGRES_DB", "baihua-database"),
        user=os.environ.get("POSTGRES_USER", "baihua_user"),
        password=os.environ.get("POSTGRES_PASSWORD", "password"),
    )
    try:
        with conn.cursor() as cur:
            cur.execute(
                "UPDATE room_requests SET created_at = now() - interval '121 hours' \
                 WHERE id = %s",
                (request_id,),
            )
        conn.commit()
    finally:
        conn.close()


# ── tests ─────────────────────────────────────────────────────────────

class RoomRequestChatTest:
    """26 scenarios for the room request endpoints and the private-room gate."""

    # ── R1 ────────────────────────────────────────────────────────────

    def test_search_by_username(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Search by full username finds the target user and only exposes
        public profile fields (id, username, nickname)."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="srcha")
        _token_b, user_b = _register_and_login(session, base_url, prefix="srchb")

        resp = session.get(
            f"{base_url}/api/v1/user/search",
            params={"username": user_b["username"]},
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        users = body["data"]["users"]
        assert len(users) >= 1
        assert users[0]["username"] == user_b["username"]

        # The search result must never leak private profile fields.
        for private_key in ("email", "phone_number", "is_active"):
            assert private_key not in users[0]

    # ── R2 ────────────────────────────────────────────────────────────

    def test_search_requires_filter(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Search without username or user_id is rejected with 400
        VALIDATION_ERROR."""
        token, _user = _register_and_login(session, base_url, prefix="srchq")

        resp = session.get(
            f"{base_url}/api/v1/user/search",
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    # ── R3 ────────────────────────────────────────────────────────────

    def test_search_by_user_id(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Search by exact user_id finds the target user."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="srchida")
        _token_b, user_b = _register_and_login(session, base_url, prefix="srchidb")

        resp = session.get(
            f"{base_url}/api/v1/user/search",
            params={"user_id": user_b["id"]},
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        users = body["data"]["users"]
        assert len(users) == 1
        assert users[0]["id"] == user_b["id"]
        assert users[0]["username"] == user_b["username"]

    # ── R4 ────────────────────────────────────────────────────────────

    def test_search_pagination(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Search pagination: three users share a searchable prefix, and
        limit=1&offset=1 returns exactly one of them."""
        token, _user = _register_and_login(session, base_url, prefix="srchpage")
        for _index in range(3):
            _register_user(session, base_url, prefix="pagination")

        resp = session.get(
            f"{base_url}/api/v1/user/search",
            params={"username": "pagination", "limit": 1, "offset": 1},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        data = body["data"]
        assert len(data["users"]) == 1
        assert data["count"] >= 3

    # ── R5 ────────────────────────────────────────────────────────────

    @pytest.mark.smoke
    def test_send_request_happy_path(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Sender A can send a room request to receiver B.

        Expect 201 with data.status == "pending" and a UUID request_id."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqa")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqb")

        resp = _send_request(session, base_url, token_a, user_b["id"])
        assert resp.status_code == 201, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        data = body["data"]
        assert data["status"] == "pending"
        assert data["sender_id"] == user_a["id"]
        assert data["receiver_id"] == user_b["id"]
        uuid.UUID(data["request_id"])

    # ── R6 ────────────────────────────────────────────────────────────

    def test_send_request_to_self(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Sending a room request to yourself is rejected with 400."""
        token, user = _register_and_login(session, base_url, prefix="rqself")

        resp = _send_request(session, base_url, token, user["id"])
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    # ── R7 ────────────────────────────────────────────────────────────

    def test_send_request_user_not_found(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Sending a room request to a random (nonexistent) user id is
        rejected with 400."""
        token, _user = _register_and_login(session, base_url, prefix="rqmissing")

        resp = _send_request(session, base_url, token, str(uuid.uuid4()))
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    # ── R8 ────────────────────────────────────────────────────────────

    def test_duplicate_pending_request(
        self, session: requests.Session, base_url: str
    ) -> None:
        """A second pending request to the same receiver is rejected with
        409 CONFLICT_ERROR while the first one is still pending."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqdupa")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqdupb")

        first = _send_request(session, base_url, token_a, user_b["id"])
        assert first.status_code == 201, first.text

        second = _send_request(session, base_url, token_a, user_b["id"])
        assert second.status_code == 409, second.text
        assert second.json()["code"] == "CONFLICT_ERROR"

    # ── R9 ────────────────────────────────────────────────────────────

    def test_message_validation(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Message validation: empty, control-character-only and over-long
        (501 bytes) messages are rejected with 400 VALIDATION_ERROR."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqmsg")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqmsgb")

        for bad_message in ("", "\x00\x01", "a" * 501):
            resp = _send_request(
                session, base_url, token_a, user_b["id"], message=bad_message
            )
            assert resp.status_code == 400, resp.text
            assert resp.json()["code"] == "VALIDATION_ERROR"

    # ── R10 ───────────────────────────────────────────────────────────

    def test_pending_list_receiver(
        self, session: requests.Session, base_url: str
    ) -> None:
        """The receiver's pending list contains the request, with the
        sender's username attached."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqpena")
        token_b, _user_b = _register_and_login(session, base_url, prefix="rqpenb")

        sent = _send_request(session, base_url, token_a, _user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/requests/pending",
            headers=_auth(token_b),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        pending = body["data"]["requests"]
        match = next(
            (item for item in pending if item["id"] == request_id), None
        )
        assert match is not None
        assert match["sender"]["username"] == user_a["username"]

    # ── R11 ───────────────────────────────────────────────────────────

    def test_sent_list_sender(
        self, session: requests.Session, base_url: str
    ) -> None:
        """The sender's sent list contains the request, with the
        receiver's username attached."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqsenta")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqsentb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/requests/sent",
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        sent_list = body["data"]["requests"]
        match = next(
            (item for item in sent_list if item["id"] == request_id), None
        )
        assert match is not None
        assert match["receiver"]["username"] == user_b["username"]

    # ── R12 ───────────────────────────────────────────────────────────

    def test_accept_creates_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Accepting a request creates a private room with exactly the
        two users as members."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqacca")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqaccb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"

        data = body["data"]
        assert data["status"] == "accepted"
        assert data["request_id"] == request_id

        room = data["room"]
        room_id = room["id"]
        uuid.UUID(room_id)
        assert room["is_group"] is False

        members = room["members"]
        assert len(members) == 2
        assert user_a["id"] in members
        assert user_b["id"] in members

    # ── R13 ───────────────────────────────────────────────────────────

    def test_accept_is_idempotent(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Accepting the same request again still returns 200 with the
        same room."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqidema")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqidemb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        first = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert first.status_code == 200, first.text
        room_id = first.json()["data"]["room"]["id"]

        second = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert second.status_code == 200, second.text
        body = second.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["status"] == "accepted"
        assert body["data"]["room"]["id"] == room_id

    # ── R14 ───────────────────────────────────────────────────────────

    def test_accept_only_receiver(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Only the receiver can accept; the sender gets 403
        FORBIDDEN_ERROR."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqonlya")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqonlyb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_a),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

    # ── R15 ───────────────────────────────────────────────────────────

    def test_accept_unknown_request(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Accepting a random (nonexistent) request id returns 404
        NOT_FOUND_ERROR."""
        token, _user = _register_and_login(session, base_url, prefix="rqunknown")

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{uuid.uuid4()}/accept",
            headers=_auth(token),
        )
        assert resp.status_code == 404, resp.text
        assert resp.json()["code"] == "NOT_FOUND_ERROR"

    # ── R16 ───────────────────────────────────────────────────────────

    def test_decline_flow(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Declining a request settles it: a later accept is rejected with
        400, and the sender's sent list keeps showing it with the terminal
        status "declined"."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqdecla")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqdeclb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        decline = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/decline",
            headers=_auth(token_b),
        )
        assert decline.status_code == 200, decline.text
        body = decline.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["status"] == "declined"

        accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert accept.status_code == 400, accept.text
        assert accept.json()["code"] == "BAD_REQUEST_ERROR"

        resp = session.get(
            f"{base_url}/api/v1/chat/rooms/requests/sent",
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        sent_list = resp.json()["data"]["requests"]
        match = next(
            (item for item in sent_list if item["id"] == request_id), None
        )
        assert match is not None
        assert match["status"] == "declined"

    # ── R17 ───────────────────────────────────────────────────────────

    def test_direct_private_room_gated(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Without an accepted request, creating a private room directly
        is rejected with 403 FORBIDDEN_ERROR."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqgatea")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqgateb")

        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": user_b["username"]},
            headers=_auth(token_a),
        )
        assert resp.status_code == 403, resp.text
        assert resp.json()["code"] == "FORBIDDEN_ERROR"

    # ── R18 ───────────────────────────────────────────────────────────

    def test_room_gate_after_accept(
        self, session: requests.Session, base_url: str
    ) -> None:
        """After an accepted request, direct room creation returns 200
        with the same room, and both users see it in their room list."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqgate2a")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqgate2b")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert accept.status_code == 200, accept.text
        room_id = accept.json()["data"]["room"]["id"]

        # Direct creation by A now passes the gate (accepted request
        # exists) and returns the existing room.
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": user_b["username"]},
            headers=_auth(token_a),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["id"] == room_id

        # Both users see the room in their room list.
        for token in (token_a, token_b):
            listing = session.get(
                f"{base_url}/api/v1/chat/rooms",
                headers=_auth(token),
            )
            assert listing.status_code == 200, listing.text
            room_ids = [room["id"] for room in listing.json()["data"]["rooms"]]
            assert room_id in room_ids

    # ── R19 ───────────────────────────────────────────────────────────

    def test_uuid_v7_room_id(
        self, session: requests.Session, base_url: str
    ) -> None:
        """The room created by an accepted request has a parseable UUID
        identifier."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rquuida")
        token_b, user_b = _register_and_login(session, base_url, prefix="rquuidb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert accept.status_code == 200, accept.text

        room_id = accept.json()["data"]["room"]["id"]
        uuid.UUID(room_id)

    # ── R20 ───────────────────────────────────────────────────────────

    def test_requests_require_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Room request endpoints reject requests without a token with
        401 AUTHENTICATION_ERROR."""
        random_id = str(uuid.uuid4())

        # create request
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests",
            json={"receiver_id": random_id, "message": "hello"},
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # pending list
        resp = session.get(f"{base_url}/api/v1/chat/rooms/requests/pending")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # sent list
        resp = session.get(f"{base_url}/api/v1/chat/rooms/requests/sent")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # accept
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{random_id}/accept"
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # decline
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{random_id}/decline"
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

        # cancel
        resp = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{random_id}/cancel"
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    # ── R21 ───────────────────────────────────────────────────────────

    def test_expired_request_does_not_block_resend(
        self, session: requests.Session, base_url: str
    ) -> None:
        """An expired pending request no longer counts against the inbox
        cap or the pending-pair idempotency, so the sender can re-send."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqexpa")
        _token_b, user_b = _register_and_login(session, base_url, prefix="rqexpb")

        first = _send_request(session, base_url, token_a, user_b["id"])
        assert first.status_code == 201, first.text
        request_id = first.json()["data"]["request_id"]

        # Backdate the request so the sender-side sweep on the next send
        # marks it expired before any pending-based checks run.
        _expire_request(request_id)

        resend = _send_request(session, base_url, token_a, user_b["id"])
        assert resend.status_code == 201, resend.text

    # ── R22 ───────────────────────────────────────────────────────────

    def test_gate_matches_encryption_flag(
        self, session: requests.Session, base_url: str
    ) -> None:
        """An accepted encrypted request only opens the gate for an
        encrypted room; creating a non-encrypted room is still blocked."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqenca")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqencb")

        sent = _send_request(
            session, base_url, token_a, user_b["id"], is_encrypted=True
        )
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert accept.status_code == 200, accept.text
        room_id = accept.json()["data"]["room"]["id"]
        assert accept.json()["data"]["room"]["is_encrypted"] is True

        # Non-encrypted direct creation stays blocked: the accepted
        # request matches the encrypted flag only.
        plain = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": user_b["username"], "is_encrypted": False},
            headers=_auth(token_a),
        )
        assert plain.status_code == 403, plain.text
        assert plain.json()["code"] == "FORBIDDEN_ERROR"

        # Encrypted direct creation returns the existing encrypted room.
        encrypted = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"username": user_b["username"], "is_encrypted": True},
            headers=_auth(token_a),
        )
        assert encrypted.status_code == 200, encrypted.text
        assert encrypted.json()["code"] == "SUCCESS"
        assert encrypted.json()["data"]["id"] == room_id

    # ── R23 ───────────────────────────────────────────────────────────

    def test_concurrent_accepts_create_one_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Two concurrent accepts of the same request both succeed and
        yield the same single private room (no duplicate rooms)."""
        from concurrent.futures import ThreadPoolExecutor

        token_a, user_a = _register_and_login(session, base_url, prefix="rqconc")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqconcb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        def _accept() -> dict:
            resp = session.post(
                f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
                headers=_auth(token_b),
            )
            assert resp.status_code == 200, resp.text
            return resp.json()["data"]

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _index: _accept(), range(2)))

        room_ids = {result["room"]["id"] for result in results}
        assert len(room_ids) == 1
        assert all(result["status"] == "accepted" for result in results)

        # Exactly one private room exists between the two users.
        listing = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=_auth(token_a),
        )
        assert listing.status_code == 200, listing.text
        rooms = listing.json()["data"]["rooms"]
        private_rooms = [
            room for room in rooms if room["id"] in room_ids and not room["is_group"]
        ]
        assert len(private_rooms) == 1

    # ── R24 ───────────────────────────────────────────────────────────

    def test_cancel_flow(
        self, session: requests.Session, base_url: str
    ) -> None:
        """The sender can cancel a pending request; afterwards the pair is
        free for a new request and the request leaves the receiver's inbox."""
        token_a, _user_a = _register_and_login(session, base_url, prefix="rqcala")
        token_b, _user_b = _register_and_login(session, base_url, prefix="rqcalb")

        sent = _send_request(session, base_url, token_a, _user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        cancel = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/cancel",
            headers=_auth(token_a),
        )
        assert cancel.status_code == 200, cancel.text
        body = cancel.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["status"] == "cancelled"

        # The receiver's inbox no longer shows it.
        inbox = session.get(
            f"{base_url}/api/v1/chat/rooms/requests/pending",
            headers=_auth(token_b),
        )
        assert inbox.status_code == 200, inbox.text
        pending_ids = [
            item["id"] for item in inbox.json()["data"]["requests"]
        ]
        assert request_id not in pending_ids

        # The pair is free again: a fresh request is accepted.
        resend = _send_request(session, base_url, token_a, _user_b["id"])
        assert resend.status_code == 201, resend.text

    # ── R25 ───────────────────────────────────────────────────────────

    def test_cancel_only_sender_and_pending(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Only the sender can cancel, and only while the request is still
        pending."""
        token_a, user_a = _register_and_login(session, base_url, prefix="rqcalauth")
        token_b, user_b = _register_and_login(session, base_url, prefix="rqcalauthb")

        sent = _send_request(session, base_url, token_a, user_b["id"])
        assert sent.status_code == 201, sent.text
        request_id = sent.json()["data"]["request_id"]

        # The receiver cannot cancel a request addressed to them.
        by_receiver = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/cancel",
            headers=_auth(token_b),
        )
        assert by_receiver.status_code == 403, by_receiver.text
        assert by_receiver.json()["code"] == "FORBIDDEN_ERROR"

        # A third user cannot cancel either.
        token_c, _user_c = _register_and_login(session, base_url, prefix="rqcalauthc")
        by_third = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/cancel",
            headers=_auth(token_c),
        )
        assert by_third.status_code == 403, by_third.text
        assert by_third.json()["code"] == "FORBIDDEN_ERROR"

        # After the request is accepted, cancelling is no longer allowed.
        accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/accept",
            headers=_auth(token_b),
        )
        assert accept.status_code == 200, accept.text

        cancel_after_accept = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{request_id}/cancel",
            headers=_auth(token_a),
        )
        assert cancel_after_accept.status_code == 400, cancel_after_accept.text
        assert cancel_after_accept.json()["code"] == "BAD_REQUEST_ERROR"

        # A random unknown request id returns 404.
        unknown = session.post(
            f"{base_url}/api/v1/chat/rooms/requests/{uuid.uuid4()}/cancel",
            headers=_auth(token_a),
        )
        assert unknown.status_code == 404, unknown.text
        assert unknown.json()["code"] == "NOT_FOUND_ERROR"

    # ── R26 ───────────────────────────────────────────────────────────

    def test_cross_direction_concurrent_accepts_create_one_room(
        self, session: requests.Session, base_url: str
    ) -> None:
        """Two pending requests in opposite directions accepted concurrently
        still yield exactly one private room (the pair-level lock prevents
        double creation)."""
        from concurrent.futures import ThreadPoolExecutor

        token_a, _user_a = _register_and_login(session, base_url, prefix="rqxda")
        token_b, _user_b = _register_and_login(session, base_url, prefix="rqxdb")

        # Both directions can hold a pending request because the unique
        # partial index only guards the same (sender, receiver) pair.
        req_ab = _send_request(session, base_url, token_a, _user_b["id"])
        assert req_ab.status_code == 201, req_ab.text
        request_ab = req_ab.json()["data"]["request_id"]

        req_ba = _send_request(session, base_url, token_b, _user_a["id"])
        assert req_ba.status_code == 201, req_ba.text
        request_ba = req_ba.json()["data"]["request_id"]

        def _accept_ab() -> dict:
            resp = session.post(
                f"{base_url}/api/v1/chat/rooms/requests/{request_ab}/accept",
                headers=_auth(token_b),
            )
            assert resp.status_code == 200, resp.text
            return resp.json()["data"]

        def _accept_ba() -> dict:
            resp = session.post(
                f"{base_url}/api/v1/chat/rooms/requests/{request_ba}/accept",
                headers=_auth(token_a),
            )
            assert resp.status_code == 200, resp.text
            return resp.json()["data"]

        with ThreadPoolExecutor(max_workers=2) as executor:
            future_ab = executor.submit(_accept_ab)
            future_ba = executor.submit(_accept_ba)
            result_ab = future_ab.result()
            result_ba = future_ba.result()

        # Both accepts must resolve to the same single room.
        room_ids = {result_ab["room"]["id"], result_ba["room"]["id"]}
        assert len(room_ids) == 1

        # Exactly one private room exists between the two users.
        listing = session.get(
            f"{base_url}/api/v1/chat/rooms",
            headers=_auth(token_a),
        )
        assert listing.status_code == 200, listing.text
        rooms = listing.json()["data"]["rooms"]
        private_rooms = [
            room for room in rooms if room["id"] in room_ids and not room["is_group"]
        ]
        assert len(private_rooms) == 1
