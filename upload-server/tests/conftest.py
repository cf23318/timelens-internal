"""Prevent synthetic tests from accidentally constructing a real cloud client."""
import pytest
@pytest.fixture(autouse=True)
def no_real_oss(monkeypatch):
    from timelens_upload import app,oss_backend,cli
    def forbidden(*args,**kwargs):raise AssertionError("Real OSS is forbidden in synthetic tests")
    monkeypatch.setattr(app,"make_bucket",forbidden)
    monkeypatch.setattr(oss_backend,"make_bucket",forbidden)
    monkeypatch.setattr(cli,"make_bucket",forbidden)
