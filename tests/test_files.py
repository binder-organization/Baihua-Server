import base64
import concurrent.futures
import hashlib
import http.client
import json
import os
import threading
import time
import uuid
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
    upload_directory = Path(os.environ["BAIHUA_TEST_FILES_DIRECTORY"]) / "uploads"
    return list(upload_directory.glob("*.part"))


def execute_statement(statement, parameters):
    connection = psycopg2.connect(
        host=os.environ["POSTGRES_HOST"],
        port=os.environ["POSTGRES_PORT"],
        dbname=os.environ["POSTGRES_DB"],
        user=os.environ["POSTGRES_USER"],
        password=os.environ["POSTGRES_PASSWORD"],
    )
    try:
        with connection.cursor() as cursor:
            cursor.execute(statement, parameters)
        connection.commit()
    finally:
        connection.close()


class TestFileChat:
    def test_failed_database_write_reclaims_newly_saved_file(
        self, session: requests.Session, base_url: str
    ):
        token, _ = _register_and_login(session, base_url, prefix="failedfilefirst")
        _, second_user = _register_and_login(session, base_url, prefix="failedfilesecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Failed file", "usernames": [second_user["username"]]},
            headers=authorization(token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        data = os.urandom(32)
        content_hash = hashlib.sha256(data).hexdigest()
        file_path = Path(os.environ["BAIHUA_TEST_FILES_DIRECTORY"]) / content_hash
        execute_statement(
            "CREATE FUNCTION reject_selected_file_attachment() RETURNS trigger AS $$ "
            "BEGIN IF NEW.original_name = 'rejected.txt' THEN "
            "RAISE EXCEPTION 'Rejected for file cleanup test'; "
            "END IF; RETURN NEW; END; $$ LANGUAGE plpgsql",
            (),
        )
        execute_statement(
            "CREATE TRIGGER reject_selected_file_attachment_before_insert "
            "BEFORE INSERT ON file_attachments FOR EACH ROW "
            "EXECUTE FUNCTION reject_selected_file_attachment()",
            (),
        )
        try:
            upload = session.post(
                f"{base_url}/api/v1/chat/rooms/{room_id}/files",
                data={"sha256": content_hash},
                files={"file": ("rejected.txt", data, "text/plain")},
                headers=authorization(token),
            )
            assert upload.status_code == 500, upload.text
            assert not file_path.exists()
            assert stored_reference_count(content_hash) is None
            assert upload_temporary_files() == []
        finally:
            execute_statement("DROP TRIGGER reject_selected_file_attachment_before_insert ON file_attachments", ())
            execute_statement("DROP FUNCTION reject_selected_file_attachment()", ())

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
        file_path = Path(os.environ["BAIHUA_TEST_FILES_DIRECTORY"]) / content_hash

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
        assert download.headers["Content-Length"] == str(len(data))
        assert download.headers["X-Content-Type-Options"] == "nosniff"
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
            file_path = Path(os.environ["BAIHUA_TEST_FILES_DIRECTORY"]) / content_hash
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

    def test_auxiliary_fields_are_bounded_and_quota_is_enforced(
        self, session: requests.Session, base_url: str
    ):
        token, user = _register_and_login(session, base_url, prefix="filelimitsfirst")
        _, second_user = _register_and_login(session, base_url, prefix="filelimitssecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "File limits", "usernames": [second_user["username"]]},
            headers=authorization(token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        data = b"bounded upload"
        content_hash = hashlib.sha256(data).hexdigest()

        oversized_hash = session.post(
            f"{base_url}/api/v1/chat/rooms/{room_id}/files",
            data={"sha256": "a" * 65},
            files={"file": ("bounded.txt", data, "text/plain")},
            headers=authorization(token),
        )
        assert oversized_hash.status_code == 400, oversized_hash.text

        oversized_metadata = session.post(
            f"{base_url}/api/v1/chat/rooms/{room_id}/files",
            data={"sha256": content_hash, "encrypted_metadata": "a" * 8193},
            files={"file": ("bounded.txt", data, "text/plain")},
            headers=authorization(token),
        )
        assert oversized_metadata.status_code == 400, oversized_metadata.text
        assert upload_temporary_files() == []

        quota_hash = "f" * 64
        quota_message_id = str(uuid.uuid4())
        execute_statement(
            "INSERT INTO stored_files (content_hash, byte_size) VALUES (%s, %s)",
            (quota_hash, 50 * 1024 * 1024 * 1024),
        )
        execute_statement(
            "INSERT INTO messages (id, room_id, sender_id, content) VALUES (%s, %s, %s, NULL)",
            (quota_message_id, room_id, user["id"]),
        )
        execute_statement(
            "INSERT INTO file_attachments (message_id, content_hash, original_name, media_type, byte_size, encrypted, encrypted_metadata) VALUES (%s, %s, %s, %s, %s, FALSE, NULL)",
            (quota_message_id, quota_hash, "quota.bin", "application/octet-stream", 50 * 1024 * 1024 * 1024),
        )
        quota_response = session.post(
            f"{base_url}/api/v1/chat/rooms/{room_id}/files",
            data={"sha256": content_hash},
            files={"file": ("bounded.txt", data, "text/plain")},
            headers=authorization(token),
        )
        assert quota_response.status_code == 413, quota_response.text
        assert "File storage quota exceeded" in quota_response.json()["message"]
        assert upload_temporary_files() == []

    def test_file_upload_is_exempt_from_regular_request_timeout(
        self, session: requests.Session, base_url: str
    ):
        token, _ = _register_and_login(session, base_url, prefix="slowfilefirst")
        _, second_user = _register_and_login(session, base_url, prefix="slowfilesecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Slow file", "usernames": [second_user["username"]]},
            headers=authorization(token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        data = b"slow upload body"
        content_hash = hashlib.sha256(data).hexdigest()
        boundary = f"baihua-{uuid.uuid4().hex}"
        prefix = (
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"sha256\"\r\n\r\n"
            f"{content_hash}\r\n--{boundary}\r\n"
            "Content-Disposition: form-data; name=\"file\"; filename=\"slow.txt\"\r\n"
            "Content-Type: text/plain\r\n\r\n"
        ).encode()
        suffix = f"\r\n--{boundary}--\r\n".encode()

        def delayed_body():
            yield prefix
            yield data[:5]
            time.sleep(2.5)
            yield data[5:]
            yield suffix

        upload = session.post(
            f"{base_url}/api/v1/chat/rooms/{room_id}/files",
            data=delayed_body(),
            headers={
                **authorization(token),
                "Content-Type": f"multipart/form-data; boundary={boundary}",
            },
        )
        assert upload.status_code == 201, upload.text

    def test_stalled_file_upload_times_out_and_removes_temporary_file(
        self, session: requests.Session, base_url: str
    ):
        token, _ = _register_and_login(session, base_url, prefix="stalledfilefirst")
        _, second_user = _register_and_login(session, base_url, prefix="stalledfilesecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Stalled file", "usernames": [second_user["username"]]},
            headers=authorization(token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        boundary = f"baihua-{uuid.uuid4().hex}"
        partial_body = (
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"stalled.txt\"\r\n"
            "Content-Type: text/plain\r\n\r\npartial file"
        ).encode()
        remaining_body = f"\r\n--{boundary}--\r\n".encode()
        connection = http.client.HTTPConnection("127.0.0.1", int(base_url.rsplit(":", 1)[1]), timeout=10)
        try:
            connection.putrequest("POST", f"/api/v1/chat/rooms/{room_id}/files")
            connection.putheader("Authorization", f"Bearer {token}")
            connection.putheader("Content-Type", f"multipart/form-data; boundary={boundary}")
            connection.putheader("Content-Length", str(len(partial_body) + len(remaining_body)))
            connection.endheaders()
            connection.send(partial_body)
            response = connection.getresponse()
            assert response.status == 408
            assert json.loads(response.read())["code"] == "REQUEST_TIMEOUT_ERROR"
        finally:
            connection.close()
        assert upload_temporary_files() == []

    def test_concurrent_upload_limit_is_enforced(
        self, session: requests.Session, base_url: str
    ):
        token, _ = _register_and_login(session, base_url, prefix="concurrentfilefirst")
        _, second_user = _register_and_login(session, base_url, prefix="concurrentfilesecond")
        room_response = session.post(
            f"{base_url}/api/v1/chat/rooms",
            json={"is_group": True, "name": "Concurrent file", "usernames": [second_user["username"]]},
            headers=authorization(token),
        )
        assert room_response.status_code == 201, room_response.text
        room_id = room_response.json()["data"]["id"]
        first_data = b"first concurrent upload"
        first_hash = hashlib.sha256(first_data).hexdigest()
        boundary = f"baihua-{uuid.uuid4().hex}"
        prefix = (
            f"--{boundary}\r\nContent-Disposition: form-data; name=\"sha256\"\r\n\r\n"
            f"{first_hash}\r\n--{boundary}\r\n"
            "Content-Disposition: form-data; name=\"file\"; filename=\"first.txt\"\r\n"
            "Content-Type: text/plain\r\n\r\n"
        ).encode()
        suffix = f"\r\n--{boundary}--\r\n".encode()
        first_body_started = threading.Event()
        release_first_body = threading.Event()

        def delayed_body():
            yield prefix
            yield first_data[:5]
            first_body_started.set()
            release_first_body.wait(timeout=10)
            yield first_data[5:]
            yield suffix

        def send_first_upload():
            return requests.post(
                f"{base_url}/api/v1/chat/rooms/{room_id}/files",
                data=delayed_body(),
                headers={
                    **authorization(token),
                    "Content-Type": f"multipart/form-data; boundary={boundary}",
                },
                timeout=15,
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            first_upload = executor.submit(send_first_upload)
            assert first_body_started.wait(timeout=5)
            time.sleep(0.2)
            second_data = b"second concurrent upload"
            second_upload = session.post(
                f"{base_url}/api/v1/chat/rooms/{room_id}/files",
                data={"sha256": hashlib.sha256(second_data).hexdigest()},
                files={"file": ("second.txt", second_data, "text/plain")},
                headers=authorization(token),
            )
            assert second_upload.status_code == 429, second_upload.text
            release_first_body.set()
            first_response = first_upload.result(timeout=10)
        assert first_response.status_code == 201, first_response.text
