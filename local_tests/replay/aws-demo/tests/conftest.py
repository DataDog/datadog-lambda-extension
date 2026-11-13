import os
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "python"))
sys.path.insert(0, os.path.join(HERE, "..", "function"))

import lambda_replay  # noqa: E402

# Recorded in Lambda by bottlecap's record mode (./invoke.sh + ./fetch.sh).
_replay = lambda_replay.start(os.path.join(HERE, "recordings"))


@pytest.fixture
def replay():
    return _replay
