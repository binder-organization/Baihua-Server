import os

import pytest
import requests


@pytest.fixture(scope="session")
def base_url():
    address = os.environ.get("BAIHUA_TEST_BASE_URL")
    if not address:
        pytest.fail("Run tests through tests/run_tests.py or set BAIHUA_TEST_BASE_URL.")
    return address


@pytest.fixture(scope="function")
def session():
    s = requests.Session()
    yield s
    s.close()
