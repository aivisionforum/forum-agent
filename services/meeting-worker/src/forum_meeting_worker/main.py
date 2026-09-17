"""Packaged sidecar entry point. stdout is reserved for protocol messages."""

import argparse
import json
import logging
import math
import sys

from . import __version__
from .protocol import Protocol, TransportError, error, frames

LOG = logging.getLogger("forum_meeting_worker")


def _positive_seconds(value: str) -> float:
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be a finite positive number")
    return number


def _write(message: dict) -> None:
    data = json.dumps(message, ensure_ascii=True, allow_nan=False, separators=(",", ":"))
    sys.stdout.buffer.write(data.encode("utf-8") + b"\n")
    sys.stdout.buffer.flush()


def main() -> int:
    parser = argparse.ArgumentParser(description="Forum meeting worker protocol bootstrap")
    parser.add_argument("--frame-timeout-seconds", type=_positive_seconds, default=10.0,
                        help="deadline for an incomplete stdin frame (default: 10)")
    parser.add_argument("--allow-model-probe", action="store_true",
                        help="enable the bounded F01 diagnostics.model_probe; not a jobs API")
    args = parser.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(name)s: %(message)s", stream=sys.stderr)
    protocol = Protocol(allow_model_probe=args.allow_model_probe)
    LOG.info("starting build %s; analysis capabilities unavailable", __version__)
    try:
        for frame in frames(sys.stdin.buffer, args.frame_timeout_seconds):
            response = protocol.handle(frame)
            if response is not None:
                _write(response)
            if protocol.stopping:
                return 0
    except TransportError as exc:
        LOG.warning("closing transport: %s", exc.code)
        _write(error(None, -32600, exc.code, str(exc)))
        return 2
    except (BrokenPipeError, OSError):
        LOG.error("stdio transport disconnected")
        return 1
    except KeyboardInterrupt:
        LOG.info("interrupted")
        return 130
    return 0
