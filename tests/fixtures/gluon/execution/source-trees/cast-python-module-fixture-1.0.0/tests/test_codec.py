import pytest

from cast_python_module_fixture import Event, decode_event, encode_event


def test_codec_is_canonical_and_round_trips() -> None:
    event = Event(code=17, message="declarative userspace")
    payload = encode_event(event)
    assert payload == b'{"code":17,"message":"declarative userspace"}'
    assert decode_event(payload) == event


@pytest.mark.parametrize(
    "payload",
    [
        b'[]',
        b'{"code":true,"message":"wrong"}',
        b'{"code":17}',
    ],
)
def test_codec_rejects_wrong_shapes(payload: bytes) -> None:
    with pytest.raises(ValueError):
        decode_event(payload)
