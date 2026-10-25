"""Bounded local diagnostics. Never record request bodies, credentials or command output."""

from contextvars import ContextVar
from datetime import datetime, timezone
import json
import logging
from logging.handlers import RotatingFileHandler
from pathlib import Path
import traceback


request_id = ContextVar('diagnostic_request_id', default=None)


class DiagnosticFormatter(logging.Formatter):
    def format(self, record):
        entry = {
            'time': datetime.now(timezone.utc).isoformat(),
            'level': record.levelname,
            'event': record.msg,
            'requestId': request_id.get(),
            'details': getattr(record, 'details', {}),
        }
        if record.exc_info:
            # Exception messages and source lines can contain secrets; retain only locations.
            entry['exceptionType'] = record.exc_info[0].__name__
            entry['frames'] = [
                {'file': frame.filename, 'line': frame.lineno, 'method': frame.name}
                for frame in traceback.extract_tb(record.exc_info[2])
            ]
        return json.dumps(entry, ensure_ascii=True)


log = logging.getLogger('agentflow.diagnostics')
log.setLevel(logging.INFO)
log.propagate = False


def open_log(path: Path) -> RotatingFileHandler:
    path.parent.mkdir(parents=True, exist_ok=True)
    handler = RotatingFileHandler(path, maxBytes=2_000_000, backupCount=4, encoding='utf-8')
    handler.setFormatter(DiagnosticFormatter())
    log.addHandler(handler)
    return handler


def event(name: str, **details) -> None:
    log.info(name, extra={'details': details})
