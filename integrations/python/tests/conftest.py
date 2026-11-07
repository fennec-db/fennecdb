"""The server the tests talk to: FENNEC_URL and FENNEC_TOKEN, as run-tests.sh
sets them for a fennec-server it starts, and FENNEC_SCHEMA_URL the schema
test's own."""

import os
import uuid

import pytest

from fennecdb import Client

URL = os.environ.get("FENNEC_URL", "http://127.0.0.1:8080")
TOKEN = os.environ.get("FENNEC_TOKEN")
SCHEMA_URL = os.environ.get("FENNEC_SCHEMA_URL")


@pytest.fixture()
def client() -> Client:
    return Client(URL, TOKEN)


@pytest.fixture()
def schema_client() -> Client:
    """A server of the schema test's own, its token FENNEC_TOKEN: the
    migrations a schema ran are recorded in the server's own `_migrations`,
    which no statement changes, so a list starts over in a new file."""
    if not SCHEMA_URL:
        pytest.skip("FENNEC_SCHEMA_URL names no server of the schema test's own")
    return Client(SCHEMA_URL, TOKEN)


def fresh(prefix: str) -> str:
    """A collection name no other test uses."""
    return f"{prefix}_{uuid.uuid4().hex[:12]}"
