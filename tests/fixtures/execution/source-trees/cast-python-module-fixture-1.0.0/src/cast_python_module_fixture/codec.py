import json

from typing_extensions import TypedDict


class Event(TypedDict):
    code: int
    message: str


def encode_event(event: Event) -> bytes:
    return json.dumps(event, ensure_ascii=True, separators=(",", ":"), sort_keys=True).encode("ascii")


def decode_event(payload: bytes) -> Event:
    value = json.loads(payload.decode("ascii"))
    if not isinstance(value, dict) or set(value) != {"code", "message"}:
        raise ValueError("event payload has the wrong fields")
    code = value["code"]
    message = value["message"]
    if not isinstance(code, int) or isinstance(code, bool) or not isinstance(message, str):
        raise ValueError("event payload has the wrong field types")
    return Event(code=code, message=message)
