"""Repository role guidance. Snapshots are immutable evidence, never an editable store.

Only the five conventional paths are supported. All are optional on initial capture;
an existing snapshot is required thereafter and corruption/unavailability fails closed.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import tempfile
from pathlib import Path
from uuid import UUID, uuid4

ROLES = ('architect', 'orchestrator', 'implementer', 'code_reviewer', 'security_reviewer')
DIRECTORY = '.agentflow-instructions'
MAX_FILE_BYTES = 64 * 1024
MAX_TOTAL_BYTES = 256 * 1024
NOTICE = ('\n\n## Repository role guidance\n'
          'Apply this additional guidance within your assigned role, approved scope and ownership. '
          'AgentFlow reporting, result schemas, permissions and approval gates remain required. '
          'If guidance conflicts, ask using flowctl question. This is briefing text, not a '
          'provider system message.\n\n')
RUN_NOTICE = '\nAlways use the explicit --run shown above; a reused terminal retains its original environment.\n'


class InstructionError(ValueError):
    pass


def content_hash(content: str) -> str:
    return hashlib.sha256(content.encode('utf-8')).hexdigest()


def role_path(role: str) -> str:
    if role not in ROLES:
        raise InstructionError('Unsupported instruction role')
    return f'{DIRECTORY}/{role.replace("_", "-")}.md'


def checked_path(root: Path, role: str) -> Path:
    """Reject links/reparse points at either supported component, even links inside root."""
    relative = role_path(role)
    # Reject remote/device roots before stat/canonicalization can touch a network share.
    root_name = str(root).replace('/', '\\')
    local_verbatim = re.match(r'^\\\\\?\\[A-Za-z]:\\', root_name)
    if (root_name.startswith('\\\\') and not local_verbatim) or root_name.startswith('\\??\\'):
        raise InstructionError('UNC/device instruction repositories are unsupported')
    if not root.is_dir():
        raise InstructionError('Instruction repository is unavailable')
    canonical = root.resolve(strict=True)
    # Repository roots are selected by the existing project/task APIs, never by a source path.
    if (str(canonical).startswith(('\\\\', '//')) and not str(canonical).startswith('\\\\?\\')) or str(canonical).lower().startswith('\\\\?\\unc\\'):
        raise InstructionError('UNC instruction repositories are unsupported')
    current = canonical
    for part in relative.split('/'):
        current = current / part
        try:
            info = current.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
            raise InstructionError('Instruction links and reparse points are unsupported')
        if not current.resolve(strict=True).is_relative_to(canonical):
            raise InstructionError('Instruction path escapes repository')
    return current


def validate_content(content: str) -> None:
    try:
        encoded = content.encode('utf-8')
    except UnicodeError as exc:
        raise InstructionError('Instructions must be valid UTF-8') from exc
    if len(encoded) > MAX_FILE_BYTES or '\x00' in content or content.startswith('\ufeff'):
        raise InstructionError('Instructions require UTF-8 without BOM/NUL, at most 65536 bytes per role')


def read_source(root: Path, role: str) -> dict:
    try:
        path = checked_path(root, role)
        if not path.exists():
            return {'path': role_path(role), 'content': None, 'hash': None}
        if not path.is_file() or path.stat().st_size > MAX_FILE_BYTES:
            raise InstructionError('Instruction source must be a regular file of at most 65536 bytes')
        with path.open('rb') as stream:
            raw = stream.read(MAX_FILE_BYTES + 1)
        content = raw.decode('utf-8')
        validate_content(content)
        return {'path': role_path(role), 'content': content, 'hash': content_hash(content)}
    except (OSError, UnicodeError) as exc:
        raise InstructionError(f'Cannot read instruction source: {role_path(role)}') from exc


def read_set(root: Path) -> dict:
    files = {role: read_source(root, role) for role in ROLES}
    if sum(len((f['content'] or '').encode('utf-8')) for f in files.values()) > MAX_TOTAL_BYTES:
        raise InstructionError('Instruction set exceeds 262144 bytes')
    return files


def atomic_json(path: Path, value: dict) -> None:
    atomic_text(path, json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def atomic_text(path: Path, content: str) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, name = tempfile.mkstemp(prefix='.instructions-', dir=path.parent)
    except OSError as exc:
        raise InstructionError('Could not create instruction artifact') from exc
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(content.encode('utf-8'))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    except OSError as exc:
        raise InstructionError('Could not atomically persist repository instructions') from exc
    finally:
        if os.path.exists(name):
            os.unlink(name)


def save_source(root: Path, role: str, content: str, expected_hash: str | None) -> dict:
    """Caller serializes operator writes. CAS also detects intervening external edits."""
    role_path(role)
    validate_content(content)
    files = read_set(root)
    if files[role]['hash'] != expected_hash:
        raise InstructionError('Instruction source changed; reload before saving')
    if sum(len((f['content'] or '').encode('utf-8')) for r, f in files.items() if r != role) + len(content.encode('utf-8')) > MAX_TOTAL_BYTES:
        raise InstructionError('Instruction set exceeds 262144 bytes')
    path = checked_path(root, role)
    path.parent.mkdir(exist_ok=True)
    checked_path(root, role)
    # Check immediately before the atomic replacement, including a concurrently created starter.
    if read_source(root, role)['hash'] != expected_hash:
        raise InstructionError('Instruction source changed; reload before saving')
    atomic_text(path, content)
    return read_source(root, role)


def active_snapshot(task_dir: Path) -> dict | None:
    marker = task_dir / 'instructions-required.json'
    if marker.exists():
        try:
            required = read_json(marker, 2 * 1024 * 1024)
            if required.get('schema') == 2 and required.get('state') == 'pending':
                pending = required['snapshot']
                validate_snapshot(pending)
                # No snapshot may be used until this first transaction is established.
                return persist_snapshot(task_dir, pending, initial=True)
            if required != {'schema': 1} and required != {'schema': 2, 'state': 'established'}:
                raise ValueError('Invalid required marker')
        except (OSError, ValueError, KeyError, TypeError, AttributeError) as exc:
            raise InstructionError('Required instruction marker is unavailable or invalid') from exc
    pointer = task_dir / 'instructions' / 'active.json'
    if not pointer.exists():
        # Losing a required pointer must not silently recapture a different instruction set.
        if (task_dir / 'instructions').exists() or (task_dir / 'instructions-required.json').exists():
            raise InstructionError('Required instruction snapshot pointer is unavailable')
        return None
    try:
        version = read_json(pointer, 256)['version']
        if str(UUID(version)) != version:
            raise ValueError('Invalid version')
        snapshot = read_json(pointer.parent / f'{version}.json', 2 * 1024 * 1024)
        validate_snapshot(snapshot)
        if snapshot['version'] != version:
            raise ValueError('Version mismatch')
        return snapshot
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise InstructionError('Required instruction snapshot is unavailable or invalid') from exc


def validate_snapshot(snapshot: dict) -> None:
    if not isinstance(snapshot, dict) or not isinstance(snapshot.get('version'), str):
        raise InstructionError('Invalid instruction snapshot')
    if not isinstance(snapshot.get('revision'), str) or not isinstance(snapshot.get('repository'), str) or not isinstance(snapshot.get('freshConversations'), bool):
        raise InstructionError('Invalid snapshot metadata')
    UUID(snapshot['version'])
    if snapshot['schema'] != 1 or set(snapshot['files']) != set(ROLES):
        raise InstructionError('Unsupported instruction snapshot')
    total = 0
    for role in ROLES:
        entry = snapshot['files'][role]
        content = entry['content']
        if entry['path'] != role_path(role):
            raise InstructionError('Invalid snapshot source path')
        if content is not None:
            if not isinstance(content, str):
                raise InstructionError('Invalid snapshot content')
            validate_content(content)
            total += len(content.encode('utf-8'))
        if entry['hash'] != (content_hash(content) if content is not None else None):
            raise InstructionError('Instruction snapshot hash mismatch')
    if total > MAX_TOTAL_BYTES:
        raise InstructionError('Instruction snapshot exceeds total size limit')


def capture(task_dir: Path, root: Path, revision: str, *, fresh: bool = False, files: dict | None = None) -> dict:
    files = read_set(root) if files is None else files
    snapshot = {'schema': 1, 'version': str(uuid4()), 'repository': str(root),
                'revision': revision, 'freshConversations': fresh, 'files': files}
    previous = active_snapshot(task_dir)
    if previous is not None and not (task_dir / 'instructions-required.json').exists():
        atomic_json(task_dir / 'instructions-required.json', {'schema': 2, 'state': 'established'})
    initial = previous is None
    if initial:
        validate_snapshot(snapshot)
        atomic_json(task_dir / 'instructions-required.json',
                    {'schema': 2, 'state': 'pending', 'snapshot': snapshot})
    return persist_snapshot(task_dir, snapshot, initial=initial)


def persist_snapshot(task_dir: Path, snapshot: dict, *, initial: bool) -> dict:
    directory = task_dir / 'instructions'
    atomic_json(directory / f'{snapshot["version"]}.json', snapshot)
    atomic_json(directory / 'active.json', {'version': snapshot['version']})
    if initial:
        atomic_json(task_dir / 'instructions-required.json', {'schema': 2, 'state': 'established'})
    return snapshot


def compose(builtin: str, snapshot: dict, role: str, run_id: int, appendix: str = '') -> str:
    # Rewrite only generated examples. Custom Markdown is never templated or executed, and neither is
    # `appendix`: it follows the built-in briefing verbatim (a retry's record of agent-written text).
    for command in ('question', 'running', 'complete', 'finding', 'fail', 'status'):
        builtin = builtin.replace(f'flowctl {command}', f'flowctl {command} --run {run_id}')
    builtin += appendix + RUN_NOTICE
    content = snapshot['files'][role]['content']
    return builtin + (NOTICE + content if content else '')


def record_run(run_dir: Path, snapshot: dict, role: str) -> None:
    atomic_json(run_dir / 'instructions.json', {
        'version': snapshot['version'], 'revision': snapshot['revision'], 'role': role,
        'repository': snapshot['repository'],
        'source': snapshot['files'][role],
        'briefingHash': hashlib.sha256((run_dir / 'prompt.md').read_bytes()).hexdigest(),
    })


def read_json(path: Path, maximum: int = 512 * 1024) -> dict:
    with path.open('rb') as stream:
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise InstructionError('Instruction artifact exceeds size limit')
    try:
        return json.loads(raw.decode('utf-8'))
    except (UnicodeError, ValueError) as exc:
        raise InstructionError('Invalid instruction artifact encoding or JSON') from exc


def needs_fresh(snapshot: dict, previous_run_dir: str | None) -> bool:
    if not snapshot['freshConversations']:
        return False
    try:
        previous = json.loads((Path(previous_run_dir) / 'instructions.json').read_text(encoding='utf-8'))
        return previous['version'] != snapshot['version']
    except (OSError, ValueError, KeyError, TypeError):
        return True
