import base64
import hashlib
import os
from pathlib import Path

import requests
import psycopg2

from test_chat import _register_and_login
from test_encrypted_chat import _active_encrypted_session


def authorization(token):
    return {"Authorization": f"Bearer {token}"}


def stored_reference_count(content_hash):
    connection = psycopg2.connect(
        host=os.environ["POSTGRES_HOST"],
        port=os.environ["POSTGRES_PORT"],
        dbname=os.environ["POSTGRES_DB"],
        user=os.environ["POSTGRES_USER"],
        password=os.environ["POSTGRES_PASSWORD"],
    )
    try:
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT reference_count FROM stored_files WHERE content_hash = %s",
                (content_hash,),
            )
            row = cursor.fetchone()
            return None if row is None else row[0]
    finally:
        connection.close()


def upload_temporary_files():
    upload_directory = Path(os.environ["BAIHUA_DIR"]) / "file_uploads"
    return list(upload_directory.glob("*.part"))


class TestFileChat:
    def test_file_hash_deduplication_permissions_and_room_cleanup(
        self, session: requests.Session, base_url: str
    ):
        first_token, first_user = _register_and_login(session, base_url, prefix="filesfirst")
        second_token, second_user = _register_and_login(session, base_url, prefix="filessecond")
        outsider_token, _ = _register_and_login(session, base_url, prefix="filesoutsider")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Shared files", "usernames": [second_user["username"]]},
            headers=authorization(first_token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        data = os.urandom(48)
        content_hash = hashlib.sha256(data).hexdigest()
        file_path = Path(os.environ["BAIHUA_DIR"]) / "files" / content_hash

        second_room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Other files", "usernames": [second_user["username"]]},
            headers=authorization(first_token),
        )
        assert second_room_response.status_code == 201, second_room_response.text
        second_room_id = second_room_response.json()["data"]["id"]

        def upload(token, supplied_hash, target_room=room_id):
            return session.post(
                f"{base_url}/api/v1/chat/rooms/{target_room}/files",
                data={"sha256": supplied_hash},
                files={"file": ("example report.txt", data, "text/plain")},
                headers=authorization(token),
            )

        assert upload(first_token, "0" * 64).status_code == 400
        assert not file_path.exists()
        assert upload_temporary_files() == []
        assert upload(outsider_token, content_hash).status_code == 403
        first_upload = upload(first_token, content_hash)
        assert first_upload.status_code == 201, first_upload.text
        assert stored_reference_count(content_hash) == 1
        second_upload = upload(second_token, content_hash)
        assert second_upload.status_code == 201, second_upload.text
        assert stored_reference_count(content_hash) == 2
        other_room_upload = upload(first_token, content_hash, second_room_id)
        assert other_room_upload.status_code == 201, other_room_upload.text
        assert stored_reference_count(content_hash) == 3
        assert file_path.read_bytes() == data
        first_message = first_upload.json()["data"]
        second_message = second_upload.json()["data"]
        assert first_message["id"] != second_message["id"]
        assert first_message["file"]["sha256"] == content_hash

        download_url = f"{base_url}{first_message['file']['download_url']}"
        assert session.get(download_url, headers=authorization(outsider_token)).status_code == 403
        download = session.get(download_url, headers=authorization(second_token))
        assert download.status_code == 200
        assert download.content == data
        assert download.headers["Content-Type"] == "text/plain"
        assert (
            download.headers["Content-Disposition"]
            == "attachment; filename*=UTF-8''example%20report.txt"
        )

        history = session.get(
            f"{base_url}/api/v1/chat/rooms/{room_id}/messages",
            headers=authorization(first_token),
        )
        assert history.status_code == 200, history.text
        assert {message["file"]["sha256"] for message in history.json()["data"]["messages"]} == {content_hash}

        for token, user in ((first_token, first_user), (second_token, second_user)):
            leave = session.delete(
                f"{base_url}/api/v1/chat/rooms/{room_id}/members/{user['id']}",
                headers=authorization(token),
            )
            assert leave.status_code == 200, leave.text
        assert file_path.exists()
        assert stored_reference_count(content_hash) == 1
        for token, user in ((first_token, first_user), (second_token, second_user)):
            leave = session.delete(
                f"{base_url}/api/v1/chat/rooms/{second_room_id}/members/{user['id']}",
                headers=authorization(token),
            )
            assert leave.status_code == 200, leave.text
        assert not file_path.exists()
        assert stored_reference_count(content_hash) is None

    def test_encrypted_file_uses_ciphertext_and_clears_after_session(
        self, session: requests.Session, base_url: str
    ):
        with _active_encrypted_session(session, base_url, "encfile") as (
            room_id, token, _, first_socket, _
        ):
            ciphertext = os.urandom(64)
            content_hash = hashlib.sha256(ciphertext).hexdigest()
            encrypted_metadata = base64.b64encode(os.urandom(48)).decode()
            file_path = Path(os.environ["BAIHUA_DIR"]) / "files" / content_hash
            upload = session.post(
                f"{base_url}/api/v1/chat/rooms/{room_id}/files",
                data={"sha256": content_hash, "encrypted_metadata": encrypted_metadata},
                files={"file": ("private.txt", ciphertext, "text/plain")},
                headers=authorization(token),
            )
            assert upload.status_code == 201, upload.text
            attachment = upload.json()["data"]["file"]
            assert attachment["name"] == "encrypted-file"
            assert attachment["media_type"] == "application/octet-stream"
            assert attachment["encrypted_metadata"] == encrypted_metadata
            assert file_path.read_bytes() == ciphertext
            assert stored_reference_count(content_hash) == 1
            download = session.get(
                f"{base_url}{attachment['download_url']}", headers=authorization(token)
            )
            assert download.content == ciphertext
            first_socket.send('{"type":"encrypt_leave","data":{"room_id":"' + room_id + '"}}')
            from test_encrypted_chat import _drain_until
            _drain_until(first_socket, "encrypt_session_ended")
            assert not file_path.exists()
            assert stored_reference_count(content_hash) is None

    def test_single_request_file_upload_can_exceed_regular_body_limit(
        self, session: requests.Session, base_url: str
    ):
        first_token, _ = _register_and_login(session, base_url, prefix="bigfilesfirst")
        second_token, second_user = _register_and_login(session, base_url, prefix="bigfilessecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Large files", "usernames": [second_user["username"]]},
            headers=authorization(first_token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        data = b"a" * (10 * 1024 * 1024 + 1024)
        content_hash = hashlib.sha256(data).hexdigest()
        upload = session.post(
            f"{base_url}/api/v1/chat/rooms/{room_id}/files",
            data={"sha256": content_hash},
            files={"file": ("large.bin", data, "application/octet-stream")},
            headers=authorization(first_token),
        )
        assert upload.status_code == 201, upload.text
        download = session.get(
            f"{base_url}{upload.json()['data']['file']['download_url']}",
            headers=authorization(second_token),
        )
        assert download.status_code == 200
        assert download.content == data
