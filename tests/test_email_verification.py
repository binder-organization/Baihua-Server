"""Exercise mail delivery and verification against the isolated test server."""

import base64
import hashlib
import os
import uuid
from urllib.parse import parse_qs, urlsplit

import psycopg2
import requests

from conftest import open_verification_link


def register_unverified(client, base_url, *, email_address=None):
    username = f"verify_{uuid.uuid4().hex[:12]}"
    email_address = email_address or f"{username}@example.test"
    response = client.post(
        f"{base_url}/api/v1/user/register",
        json={"username": username, "email": email_address, "password": "P@ssw0rd!"},
    )
    assert response.status_code == 201, response.text
    assert response.json()["data"]["user"]["email_verified_at"] is None
    return username, email_address, response.json()["data"]["user"]["id"]


def database_connection():
    return psycopg2.connect(
        host=os.environ["POSTGRES_HOST"],
        port=os.environ["POSTGRES_PORT"],
        user=os.environ["POSTGRES_USER"],
        password=os.environ["POSTGRES_PASSWORD"],
        dbname=os.environ["POSTGRES_DB"],
    )


class TestEmailVerification:
    def test_registration_requires_verification_and_uses_one_time_token(
        self, raw_session: requests.Session, base_url: str, mail_capture
    ):
        username, email_address, user_id = register_unverified(raw_session, base_url)
        login = raw_session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": username, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 401, login.text
        assert login.json()["message"] == (
            "Authentication failed: Email address is not verified."
        )

        link = mail_capture.wait_for_link(email_address)
        token = parse_qs(urlsplit(link).query)["token"][0]
        token_bytes = base64.urlsafe_b64decode(token + "=" * (-len(token) % 4))
        with database_connection() as connection:
            with connection.cursor() as cursor:
                cursor.execute(
                    "SELECT token_hash FROM email_verification_tokens WHERE user_id = %s",
                    (user_id,),
                )
                stored_hash = bytes(cursor.fetchone()[0])
        assert stored_hash == hashlib.sha256(token_bytes).digest()
        assert stored_hash != token_bytes

        verification = open_verification_link(raw_session, base_url, link)
        assert verification.status_code == 200, verification.text
        assert verification.json()["code"] == "SUCCESS"
        replay = open_verification_link(raw_session, base_url, link)
        assert replay.status_code == 400, replay.text
        assert replay.json()["code"] == "VALIDATION_ERROR"

        login = raw_session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": username, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 200, login.text
        assert login.json()["data"]["user"]["email_verified_at"] is not None

    def test_resending_replaces_previous_link_and_respects_send_limit(
        self, raw_session: requests.Session, base_url: str, mail_capture
    ):
        username, email_address, _ = register_unverified(raw_session, base_url)
        first_link = mail_capture.wait_for_link(email_address)
        resend = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": email_address, "username": username},
        )
        assert resend.status_code == 200, resend.text
        second_link = mail_capture.wait_for_link(email_address, count=2)
        assert first_link != second_link

        limited = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": email_address, "username": username},
        )
        assert limited.status_code == 200, limited.text
        assert len(mail_capture.links_for(email_address)) == 2
        assert open_verification_link(raw_session, base_url, first_link).status_code == 400
        assert open_verification_link(raw_session, base_url, second_link).status_code == 200

    def test_expired_link_cannot_verify_account(
        self, raw_session: requests.Session, base_url: str, mail_capture
    ):
        username, email_address, user_id = register_unverified(raw_session, base_url)
        link = mail_capture.wait_for_link(email_address)
        with database_connection() as connection:
            with connection.cursor() as cursor:
                cursor.execute(
                    "UPDATE email_verification_tokens "
                    "SET expires_at = now() - interval '1 second' WHERE user_id = %s",
                    (user_id,),
                )
        verification = open_verification_link(raw_session, base_url, link)
        assert verification.status_code == 400, verification.text
        login = raw_session.post(
            f"{base_url}/api/v1/user/login",
            json={"username": username, "password": "P@ssw0rd!"},
        )
        assert login.status_code == 401, login.text

    def test_invalid_requests_do_not_send_mail(
        self, raw_session: requests.Session, base_url: str, mail_capture
    ):
        email_address = f"invalid_{uuid.uuid4().hex[:12]}@example.test"
        missing_token = raw_session.get(f"{base_url}/api/v1/user/verify-email")
        assert missing_token.status_code == 400, missing_token.text
        malformed_token = raw_session.get(
            f"{base_url}/api/v1/user/verify-email?token=invalid"
        )
        assert malformed_token.status_code == 400, malformed_token.text
        invalid_email = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": "invalid"},
        )
        assert invalid_email.status_code == 400, invalid_email.text
        unknown_email = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": email_address},
        )
        assert unknown_email.status_code == 200, unknown_email.text
        assert mail_capture.links_for(email_address) == []

    def test_shared_email_requires_username_for_resending(
        self, raw_session: requests.Session, base_url: str, mail_capture
    ):
        email_address = f"shared_{uuid.uuid4().hex[:12]}@example.test"
        first_username, _, first_user_id = register_unverified(
            raw_session, base_url, email_address=email_address
        )
        second_username, _, second_user_id = register_unverified(
            raw_session, base_url, email_address=email_address
        )
        assert len(mail_capture.links_for(email_address)) == 2
        with database_connection() as connection:
            with connection.cursor() as cursor:
                cursor.execute(
                    "DELETE FROM email_verification_sends "
                    "WHERE user_id IN (%s, %s)",
                    (first_user_id, second_user_id),
                )
        resend = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": email_address},
        )
        assert resend.status_code == 200, resend.text
        assert len(mail_capture.links_for(email_address)) == 2
        targeted = raw_session.post(
            f"{base_url}/api/v1/user/resend-verification",
            json={"email": email_address, "username": first_username},
        )
        assert targeted.status_code == 200, targeted.text
        assert len(mail_capture.links_for(email_address)) == 3
        assert second_username != first_username
