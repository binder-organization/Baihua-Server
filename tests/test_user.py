import re
import uuid

import pytest
import requests


def _unique(name: str) -> str:
    """Generate a unique username/email to avoid cross-run conflicts."""
    suffix = uuid.uuid4().hex[:8]
    return f"{name}_{suffix}"


class TestRegister:
    VALIDATION_FIELDS = [
        ("username", {}, "Username"),
        ("email", {"username": "testuser"}, "Email"),
        ("password", {"username": "testuser", "email": "tests@example.com"}, "Password"),
    ]

    def test_missing_username(self, session: requests.Session, base_url: str):
        """Should reject register with empty body."""
        resp = session.post(f"{base_url}/api/v1/user/register", json={})
        assert resp.status_code == 400

        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "username is required" in body["message"].lower()

    def test_missing_email(self, session: requests.Session, base_url: str):
        """Should reject register with only username."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": "testuser"},
        )
        assert resp.status_code == 400

        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "email is required" in body["message"].lower()

    def test_missing_password(self, session: requests.Session, base_url: str):
        """Should reject register without password."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": "testuser", "email": "tests@example.com"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "password is required" in body["message"].lower()

    def test_register_success(self, session: requests.Session, base_url: str):
        """Should create a new user and return 201."""
        uname = _unique("regtest")
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 201, resp.text

        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["data"]["user"]["username"] == uname
        assert body["data"]["user"]["email"] == f"{uname}@example.com"

    def test_duplicate_username(self, session: requests.Session, base_url: str):
        """Should reject registration with an existing username."""
        uname = _unique("dupuser")
        # Register the first time
        session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        # Register with the same username but different email
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}_other@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 400
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_empty_username_string(self, session: requests.Session, base_url: str):
        """Should reject register with empty username string."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": "", "email": "a@b.com", "password": "P@ssw0rd!"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "username is required" in body["message"].lower()

    def test_empty_email_string(self, session: requests.Session, base_url: str):
        """Should reject register with empty email string."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": "testuser", "email": "", "password": "P@ssw0rd!"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "email is required" in body["message"].lower()

    def test_empty_password_string(self, session: requests.Session, base_url: str):
        """Should reject register with empty password string."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": "testuser", "email": "a@b.com", "password": ""},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "password is required" in body["message"].lower()


class TestLogin:
    def test_missing_username(self, session: requests.Session, base_url: str):
        """Login without username should fail validation."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"password": "somepass"},
        )
        assert resp.status_code == 400

        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "username is required" in body["message"].lower()

    def test_missing_password(self, session: requests.Session, base_url: str):
        """Login without password should fail validation."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": "someuser"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "password is required" in body["message"].lower()

    def test_login_without_email_succeeds_validation(self, session: requests.Session, base_url: str):
        """
        Login should NOT require email.

        This was the bug that motivated the middleware refactor:
        validate_user() required BOTH username AND email on all /api/v1/user/*
        routes, which broke login. After the split, validate_login should
        only check username + password and should *not* reject requests
        that omit email.
        """
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": "nonexistent", "password": "somepass"},
        )
        # If validation passes but credentials are wrong → 401, not 400
        assert resp.status_code == 401, (
            f"Expected 401 (wrong credentials, not 400 for missing email). "
            f"Got {resp.status_code}: {resp.text}"
        )

    def test_empty_body(self, session: requests.Session, base_url: str):
        """Login with empty body should fail validation."""
        resp = session.post(f"{base_url}/api/v1/user/login", json={})
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "username is required" in body["message"].lower()

    def test_empty_username_string(self, session: requests.Session, base_url: str):
        """Should reject login with empty username string."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": "", "password": "somepass"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "username is required" in body["message"].lower()

    def test_empty_password_string(self, session: requests.Session, base_url: str):
        """Should reject login with empty password string."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": "someuser", "password": ""},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "password is required" in body["message"].lower()

    def test_login_wrong_credentials(self, session: requests.Session, base_url: str):
        """Should return 401 for wrong password."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": "nobody_will_register_this", "password": "wrongpass"},
        )
        assert resp.status_code == 401

        body = resp.json()
        assert body["code"] == "AUTHENTICATION_ERROR"

class TestValidationMiddleware:
    """Tests for shared middleware behavior (validate_json_body)."""

    def test_register_wrong_content_type(self, session: requests.Session, base_url: str):
        """Register with wrong Content-Type should be rejected before handler."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            data="not json",
            headers={"Content-Type": "text/plain"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "content-type" in body["message"].lower()

    def test_register_invalid_json(self, session: requests.Session, base_url: str):
        """Register with malformed JSON should be rejected."""
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            data="not valid json{{{",
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "INVALID_JSON_ERROR"

    def test_login_wrong_content_type(self, session: requests.Session, base_url: str):
        """Login with wrong Content-Type should be rejected before handler."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            data="not json",
            headers={"Content-Type": "text/plain"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "content-type" in body["message"].lower()

    def test_login_invalid_json(self, session: requests.Session, base_url: str):
        """Login with malformed JSON should be rejected."""
        resp = session.post(
            f"{base_url}/api/v1/user/login",
            data="not valid json{{{",
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 400
        body = resp.json()
        assert body["code"] == "INVALID_JSON_ERROR"


class TestLoginFullFlow:
    @pytest.mark.smoke
    def test_register_then_login_success(self, session: requests.Session, base_url: str):
        """Full flow: register a user, then login, expect token back."""
        uname = _unique("flowtest")
        email = f"{uname}@example.com"
        password = "C0rrect!pass"

        # Register
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={"username": uname, "email": email, "password": password},
        )
        assert reg.status_code == 201, reg.text

        # Login (without email — verifying the fix)
        log = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": password},
        )
        assert log.status_code == 200, log.text

        body = log.json()
        assert body["code"] == "SUCCESS"
        assert "token" in body["data"]
        assert body["data"]["user"]["username"] == uname


def _auth(token: str) -> dict:
    return {"Authorization": f"Bearer {token}"}


class TestRegisterUuidUsernameRejected:
    """Registration must reject usernames that parse as a UUID."""

    def test_uuid_username_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        uname = str(uuid.uuid4())
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 400, resp.text
        body = resp.json()
        assert body["code"] == "VALIDATION_ERROR"
        assert "uuid" in body["message"].lower()

    def test_uuid_like_username_still_allowed(
        self, session: requests.Session, base_url: str
    ) -> None:
        # A hyphenated name with non-hex letters is not parseable as a
        # UUID text form, so it must remain a legal username.
        uname = _unique("uuidish")
        resp = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert resp.status_code == 201, resp.text


class TestPublicProfile:
    """GET /api/v1/user/{user} returns the public profile by id or name."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_get_by_id_without_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        _, user = self._register_and_login(
            session, base_url, prefix="pubprof"
        )
        resp = session.get(f"{base_url}/api/v1/user/{user['id']}")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_get_by_id_returns_public_fields(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="pubprof"
        )
        resp = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(token)
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"
        data = body["data"]["user"]
        assert data["id"] == user["id"]
        assert data["username"] == user["username"]
        assert data["nickname"] is None
        for private_key in ("email", "phone_number", "is_active", "created_at"):
            assert private_key not in data

    def test_get_by_username_matches_by_id(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="pubprof"
        )
        by_id = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(token)
        )
        by_name = session.get(
            f"{base_url}/api/v1/user/{user['username']}", headers=_auth(token)
        )
        assert by_id.status_code == 200, by_id.text
        assert by_name.status_code == 200, by_name.text
        assert by_id.json()["data"]["user"]["id"] == by_name.json()["data"]["user"]["id"]

    def test_get_unknown_user_returns_404(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="pubprof"
        )
        resp = session.get(
            f"{base_url}/api/v1/user/{uuid.uuid4()}", headers=_auth(token)
        )
        assert resp.status_code == 404, resp.text
        assert resp.json()["code"] == "NOT_FOUND_ERROR"


class TestUpdateProfile:
    """PATCH /api/v1/user/me updates the caller's nickname and phone."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_patch_without_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        resp = session.patch(
            f"{base_url}/api/v1/user/me", json={"nickname": "X"}
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_update_nickname(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"nickname": "Gavin"},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]["user"]
        assert data["nickname"] == "Gavin"
        assert data["phone_number"] is None

    def test_update_phone_e164(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"phone_number": "+8613800000000"},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]["user"]
        assert data["phone_number"] == "+8613800000000"
        assert data["nickname"] is None

    def test_partial_update_keeps_other_field(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        first = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"nickname": "Gavin"},
            headers=_auth(token),
        )
        assert first.status_code == 200, first.text

        second = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"phone_number": "+8613800000000"},
            headers=_auth(token),
        )
        assert second.status_code == 200, second.text
        data = second.json()["data"]["user"]
        assert data["nickname"] == "Gavin"
        assert data["phone_number"] == "+8613800000000"

    def test_null_clears_nickname(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        set_resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"nickname": "Gavin"},
            headers=_auth(token),
        )
        assert set_resp.status_code == 200, set_resp.text

        clear_resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"nickname": None},
            headers=_auth(token),
        )
        assert clear_resp.status_code == 200, clear_resp.text
        assert clear_resp.json()["data"]["user"]["nickname"] is None

    def test_empty_body_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me", json={}, headers=_auth(token)
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "BAD_REQUEST_ERROR"

    def test_nickname_too_long_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"nickname": "x" * 41},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_phone_not_e164_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="updprof"
        )
        for bad in ("13800000000", "+08613800000000", "+1234abc", "++8613800000000"):
            resp = session.patch(
                f"{base_url}/api/v1/user/me",
                json={"phone_number": bad},
                headers=_auth(token),
            )
            assert resp.status_code == 400, f"{bad}: {resp.text}"
            assert resp.json()["code"] == "VALIDATION_ERROR"


class TestChangePassword:
    """PATCH /api/v1/user/me/password changes the caller's password."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_change_without_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        resp = session.patch(
            f"{base_url}/api/v1/user/me/password",
            json={"old_password": "P@ssw0rd!", "new_password": "N3w-pass!"},
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_wrong_old_password_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="chgpw"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me/password",
            json={"old_password": "Wrong-password!", "new_password": "N3w-pass!"},
            headers=_auth(token),
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_empty_passwords_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="chgpw"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me/password",
            json={"old_password": "", "new_password": ""},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_change_password_rotates_login(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="chgpw"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me/password",
            json={"old_password": "P@ssw0rd!", "new_password": "N3w-pass!"},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        assert resp.json()["code"] == "SUCCESS"
        assert resp.json()["data"] is None

        old_login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": user["username"], "password": "P@ssw0rd!"},
        )
        assert old_login.status_code == 401, old_login.text

        new_login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": user["username"], "password": "N3w-pass!"},
        )
        assert new_login.status_code == 200, new_login.text


class TestLogout:
    """POST /api/v1/user/me/logout revokes the current token on all devices."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_logout_without_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        resp = session.post(f"{base_url}/api/v1/user/me/logout")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_logout_invalidates_token_and_relogin_works(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="logout"
        )
        resp = session.post(
            f"{base_url}/api/v1/user/me/logout", headers=_auth(token)
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["data"] is None

        # The pre-logout token must now be rejected everywhere.
        protected = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(token)
        )
        assert protected.status_code == 401, protected.text
        assert protected.json()["code"] == "AUTHENTICATION_ERROR"

        # A fresh login issues a token that works again.
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": user["username"], "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        new_token = login.json()["data"]["token"]
        ok = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(new_token)
        )
        assert ok.status_code == 200, ok.text


class TestDeleteAccount:
    """DELETE /api/v1/user/me deletes the caller after password verification."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_delete_without_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        resp = session.delete(f"{base_url}/api/v1/user/me")
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_delete_without_password_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="delacct"
        )
        resp = session.delete(
            f"{base_url}/api/v1/user/me", json={}, headers=_auth(token)
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "INVALID_JSON_ERROR"

    def test_delete_with_empty_password_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="delacct"
        )
        resp = session.delete(
            f"{base_url}/api/v1/user/me",
            json={"password": ""},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_delete_with_wrong_password_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="delacct"
        )
        resp = session.delete(
            f"{base_url}/api/v1/user/me",
            json={"password": "Wrong-password!"},
            headers=_auth(token),
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_delete_account_success(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="delacct"
        )
        resp = session.delete(
            f"{base_url}/api/v1/user/me",
            json={"password": "P@ssw0rd!"},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert body["data"] is None

        # The deleted account no longer exists: the old token is rejected...
        protected = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(token)
        )
        assert protected.status_code == 401, protected.text
        assert protected.json()["code"] == "AUTHENTICATION_ERROR"

        # ...and the credentials cannot log in again.
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": user["username"], "password": "P@ssw0rd!"},
        )
        assert login.status_code == 401, login.text
        assert login.json()["code"] == "AUTHENTICATION_ERROR"


class TestProfileBioAvatar:
    """PATCH /api/v1/user/me supports the public bio and avatar fields."""

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_set_bio_and_avatar(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="bioavt"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={
                "bio": "Rust enjoyer. 百花用户.",
                "avatar": "https://example.com/avatar.png",
            },
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]["user"]
        assert data["bio"] == "Rust enjoyer. 百花用户."
        assert data["avatar"] == "https://example.com/avatar.png"

    def test_public_profile_shows_bio_and_avatar(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, user = self._register_and_login(
            session, base_url, prefix="bioavt"
        )
        set_resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={
                "bio": "Hello from the tests.",
                "avatar": "http://example.com/avatar.png",
            },
            headers=_auth(token),
        )
        assert set_resp.status_code == 200, set_resp.text

        resp = session.get(
            f"{base_url}/api/v1/user/{user['id']}", headers=_auth(token)
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]["user"]
        assert data["bio"] == "Hello from the tests."
        assert data["avatar"] == "http://example.com/avatar.png"

    def test_null_clears_bio_and_avatar(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="bioavt"
        )
        set_resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={
                "bio": "Temporary bio.",
                "avatar": "https://example.com/avatar.png",
            },
            headers=_auth(token),
        )
        assert set_resp.status_code == 200, set_resp.text

        clear_resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"bio": None, "avatar": None},
            headers=_auth(token),
        )
        assert clear_resp.status_code == 200, clear_resp.text
        data = clear_resp.json()["data"]["user"]
        assert data["bio"] is None
        assert data["avatar"] is None

    def test_bio_too_long_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="bioavt"
        )
        resp = session.patch(
            f"{base_url}/api/v1/user/me",
            json={"bio": "x" * 201},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_avatar_without_http_scheme_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(
            session, base_url, prefix="bioavt"
        )
        for bad in ("ftp://example.com/avatar.png", "example.com/avatar.png"):
            resp = session.patch(
                f"{base_url}/api/v1/user/me",
                json={"avatar": bad},
                headers=_auth(token),
            )
            assert resp.status_code == 400, f"{bad}: {resp.text}"
            assert resp.json()["code"] == "VALIDATION_ERROR"


class TestAvatarUpload:
    """POST /api/v1/user/me/avatar uploads an image stored under /static/avatars/."""

    PNG_PAYLOAD = b"\x89PNG\r\n\x1a\n" + b"x" * 100

    @staticmethod
    def _register_and_login(
        session: requests.Session, base_url: str, *, prefix: str
    ) -> tuple[str, dict]:
        uname = _unique(prefix)
        reg = session.post(
            f"{base_url}/api/v1/user/register",
            json={
                "username": uname,
                "email": f"{uname}@example.com",
                "password": "P@ssw0rd!",
            },
        )
        assert reg.status_code == 201, reg.text
        login = session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": uname, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        body = login.json()
        return body["data"]["token"], body["data"]["user"]

    def test_upload_avatar_success(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(session, base_url, prefix="avtup")
        resp = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"file": ("a.png", self.PNG_PAYLOAD, "image/png")},
            headers=_auth(token),
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["code"] == "SUCCESS"
        assert re.match(
            r"^/static/avatars/[0-9a-f-]{36}\.png$", body["data"]["user"]["avatar"]
        ), body["data"]["user"]["avatar"]

    def test_serve_uploaded_avatar(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(session, base_url, prefix="avtsrv")
        upload = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"file": ("a.png", self.PNG_PAYLOAD, "image/png")},
            headers=_auth(token),
        )
        assert upload.status_code == 200, upload.text
        avatar_path = upload.json()["data"]["user"]["avatar"]

        resp = session.get(f"{base_url}{avatar_path}")
        assert resp.status_code == 200, resp.text
        assert resp.headers["Content-Type"] == "image/png"
        assert resp.content == self.PNG_PAYLOAD

    def test_upload_requires_auth(
        self, session: requests.Session, base_url: str
    ) -> None:
        resp = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"file": ("a.png", self.PNG_PAYLOAD, "image/png")},
        )
        assert resp.status_code == 401, resp.text
        assert resp.json()["code"] == "AUTHENTICATION_ERROR"

    def test_upload_oversize_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(session, base_url, prefix="avtbig")
        resp = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"file": ("big.png", b"x" * (2 * 1024 * 1024 + 1), "image/png")},
            headers=_auth(token),
        )
        assert resp.status_code == 413, resp.text
        assert resp.json()["code"] == "PAYLOAD_TOO_LARGE_ERROR"

    def test_upload_non_image_rejected(
        self, session: requests.Session, base_url: str
    ) -> None:
        token, _ = self._register_and_login(session, base_url, prefix="avttxt")
        resp = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"file": ("a.txt", b"hello", "text/plain")},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"

    def test_upload_missing_field(
        self, session: requests.Session, base_url: str
    ) -> None:
        """
        A multipart body without the 'file' field must be rejected.

        Note: passing files={} sends no multipart Content-Type at all, which
        axum rejects as an invalid boundary before the handler runs (plain
        400, not VALIDATION_ERROR), so a real multipart body containing an
        unrelated field is used to exercise the handler's missing-field branch.
        """
        token, _ = self._register_and_login(session, base_url, prefix="avtnof")
        resp = session.post(
            f"{base_url}/api/v1/user/me/avatar",
            files={"unexpected": ("a.txt", b"hello", "text/plain")},
            headers=_auth(token),
        )
        assert resp.status_code == 400, resp.text
        assert resp.json()["code"] == "VALIDATION_ERROR"
