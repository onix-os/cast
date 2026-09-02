import sys

from .codec import Event, decode_event, encode_event


IDENTITY = "cast python module fixture: offline PEP 517 wheel"


def main() -> int:
    if sys.argv[1:] != ["--self-test"]:
        print("usage: cast-python-module-fixture --self-test", file=sys.stderr)
        return 64
    expected = Event(code=17, message="declarative userspace")
    if decode_event(encode_event(expected)) != expected:
        return 1
    print(IDENTITY)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
