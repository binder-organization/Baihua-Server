import os
import re
import time
from email import policy
from email.parser import BytesParser
from pathlib import Path
from urllib.parse import urlsplit

import pytest
import requests


@pytest.fixture(scope="session")
def base_url():
    address = os.environ.get("BAIHUA_TEST_BASE_URL")
    if not address:
        pytest.fail("Run tests through tests/run_tests.py or set BAIHUA_TEST_BASE_URL.")
    return address


@pytest.fixture(scope="function")
def session(base_url, mail_capture):
    client = VerifyingSession(base_url, mail_capture)
    yield client
    client.close()


@pytest.fixture(scope="function")
def raw_session():
    client = requests.Session()
    yield client
    client.close()


@pytest.fixture(scope="session")
def mail_capture():
    directory = os.environ.get("BAIHUA_TEST_MAIL_MESSAGES_DIRECTORY")
    if not directory:
        pytest.fail("Run tests through tests/run_tests.py to capture verification mail.")
    return MailCapture(Path(directory))


class MailCapture:
    def __init__(self, directory):
        self.directory = directory

    def links_for(self, email_address):
        links = []
        for message_path in sorted(self.directory.glob("*.eml")):
            message = BytesParser(policy=policy.default).parsebytes(
                message_path.read_bytes()
            )
            if email_address.lower() not in str(message["To"]).lower():
                continue
            content = message.get_body(preferencelist=("plain",))
            body = content.get_content() if content is not None else message.get_content()
            match = re.search(
                r"https://[^\s]+/api/v1/user/verify-email\?token=[A-Za-z0-9_-]+",
                body,
            )
            if match:
                links.append(match.group(0))
        return links

    def wait_for_link(self, email_address, count=1):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            links = self.links_for(email_address)
            if len(links) >= count:
                return links[-1]
            time.sleep(0.05)
        pytest.fail(f"No verification message reached {email_address}.")


def open_verification_link(client, base_url, link):
    address = urlsplit(link)
    return client.get(f"{base_url}{address.path}?{address.query}")


class VerifyingSession(requests.Session):
    def __init__(self, base_url, mail_capture):
        super().__init__()
        self.base_url = base_url
        self.mail_capture = mail_capture

    def post(self, url, **kwargs):
        response = super().post(url, **kwargs)
        if url.endswith("/api/v1/user/register") and response.status_code == 201:
            email_address = kwargs["json"]["email"]
            link = self.mail_capture.wait_for_link(email_address)
            verification = open_verification_link(self, self.base_url, link)
            assert verification.status_code == 200, verification.text
        return response
