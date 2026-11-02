"""Jira Cloud REST client for issue search and syncing a ticket's context onto local disk.

AgentFlow otherwise leaves Jira to the harness's own MCP (see README "MCP prerequisites").
This module covers what is worth doing from the backend instead: typeahead search when creating
a task, and syncing the ticket's description, comments and attachment bytes (plus keyframes of
video attachments) onto local disk so the Architect can read them directly instead of via its MCP
or a browser. Linked issues, custom fields and write-back are still the harness's job.

Built fresh from config on every call (not cached on app state) so a config reload picks up
new credentials immediately.
"""

from __future__ import annotations

import html
import json
import os
import re
import secrets
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import httpx

from ..config import JiraConfig
from ..diagnostics import event
from . import media_frames


class JiraError(RuntimeError):
    pass


@dataclass(frozen=True)
class TransitionOutcome:
    """What `JiraService.transition` did. `result` is "moved", "already" (the ticket is in the
    target status), "not-allowed" (its status is not in the from-list) or "unavailable" (Jira
    offers no transition to the target from its status). `current` is the status it was in."""
    result: str
    current: str

    @property
    def moved(self) -> bool:
        return self.result == "moved"


def status_allowed(current: str, from_statuses: list[str]) -> bool:
    """Case-insensitive membership; "*" in from_statuses matches any status."""
    allowed = {s.casefold() for s in from_statuses}
    return "*" in allowed or current.casefold() in allowed


@dataclass
class JiraIssueSummary:
    key: str
    summary: str
    icon_url: str | None = None
    section: str | None = None


@dataclass
class SkippedAttachment:
    id: str
    filename: str
    reason: str


@dataclass
class TicketSync:
    """What one sync_ticket call changed on disk, for the task's event log."""
    added: list[str] = field(default_factory=list)
    kept: list[str] = field(default_factory=list)
    removed: list[str] = field(default_factory=list)
    skipped: list[SkippedAttachment] = field(default_factory=list)
    frames: dict[str, int] = field(default_factory=dict)
    frame_errors: dict[str, str] = field(default_factory=dict)
    comments_error: str | None = None
    title: str | None = None

    @property
    def changed(self) -> bool:
        return bool(self.added or self.removed or self.skipped or self.frames or self.frame_errors
                    or self.comments_error)

    def summary(self) -> str:
        parts = [f"+{len(self.added)} new", f"{len(self.kept)} unchanged"]
        if self.removed:
            parts.append(f"{len(self.removed)} removed from the ticket")
        if self.skipped:
            parts.append(f"{len(self.skipped)} skipped (" + "; ".join(
                f"{s.id} {s.filename}: {s.reason}" for s in self.skipped) + ")")
        if self.frames:
            parts.append(f"keyframes from {len(self.frames)} video(s)")
        if self.frame_errors:
            parts.append(f"{len(self.frame_errors)} video(s) without keyframes")
        if self.comments_error:
            parts.append(f"comments not fetched ({self.comments_error})")
        return ", ".join(parts)


def build_service(jira_config: JiraConfig) -> "JiraService | None":
    """None when Jira integration isn't configured; callers treat that as a no-op."""
    if not jira_config.enabled or not jira_config.base_url or not jira_config.api_token:
        return None
    return JiraService(jira_config)


class JiraService:
    def __init__(self, config: JiraConfig, transport: httpx.BaseTransport | None = None) -> None:
        self._base_url = (config.base_url or "").rstrip("/")
        self._email = config.email
        self._token = config.api_token or ""
        self._config = config
        self._validate_url(self._base_url, initial=True)
        # Injected by tests to fake Jira's responses without real network access.
        self._transport = transport

    def _client(self, timeout: float) -> httpx.AsyncClient:
        # Attachment content on Jira Cloud 303-redirects to api.media.atlassian.com with its own
        # pre-signed token in the query string; httpx does not follow redirects by default.
        return httpx.AsyncClient(timeout=timeout, transport=self._transport, follow_redirects=False)

    def _validate_url(self, value: str, *, initial: bool = False) -> httpx.URL:
        try:
            url = httpx.URL(value)
        except (httpx.InvalidURL, TypeError, ValueError) as exc:
            raise JiraError("Invalid attachment destination") from exc
        base = httpx.URL(self._base_url)
        same_origin = (url.scheme, url.host, url.port) == (base.scheme, base.host, base.port)
        allowed = same_origin or (not initial and url.host in self._config.attachment_hosts and url.port in (None, 443))
        if url.scheme != "https" or url.userinfo or not url.host or not allowed:
            raise JiraError("Untrusted Jira attachment destination")
        return url

    async def _download(self, client, url: str, dest: Path, limit: int) -> int:
        current = self._validate_url(url, initial=True)
        for _ in range(6):
            self._validate_url(str(current))
            base = httpx.URL(self._base_url)
            same_origin = (current.scheme, current.host, current.port) == (base.scheme, base.host, base.port)
            headers, auth = self._auth() if same_origin else ({}, None)
            async with client.stream("GET", current, headers={"Accept-Encoding": "identity", **headers}, auth=auth) as response:
                if response.is_redirect:
                    location = response.headers.get("location")
                    if not location:
                        raise JiraError("Attachment redirect has no destination")
                    current = current.join(location)
                    continue
                response.raise_for_status()
                if response.headers.get("content-encoding", "identity").lower() != "identity":
                    raise JiraError("Encoded attachment responses are not accepted")
                size = 0
                try:
                    with dest.open("xb") as output:
                        async for chunk in response.aiter_bytes(chunk_size=65536):
                            size += len(chunk)
                            if size > limit:
                                raise JiraError("Attachment download exceeds configured byte limit")
                            output.write(chunk)
                except FileExistsError:
                    raise JiraError("Attachment destination already exists") from None
                except BaseException:
                    dest.unlink(missing_ok=True)
                    raise
                return size
        raise JiraError("Too many attachment redirects")

    def _auth(self) -> tuple[dict[str, str], tuple[str, str] | None]:
        # Cloud: Basic email + API token. Server/Data Center PAT: Bearer, no email configured.
        if self._email:
            return {}, (self._email, self._token)
        return {"Authorization": f"Bearer {self._token}"}, None

    async def search_issues(self, query: str, max_results: int = 20) -> list[JiraIssueSummary]:
        query = query.strip()
        if not query:
            return []
        headers, auth = self._auth()
        async with self._client(10.0) as client:
            try:
                response = await client.get(
                    f"{self._base_url}/rest/api/3/issue/picker",
                    # Without currentJQL the picker only searches the account's view history
                    # ("History Search"), so tickets never opened in Jira would not be found.
                    params={"query": query, "currentJQL": "order by updated DESC"},
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
        self._raise_for_status(response, "search")
        results: list[JiraIssueSummary] = []
        seen: set[str] = set()
        for section in response.json().get("sections", []):
            section_label = section.get("label") or None
            for issue in section.get("issues", []):
                key = issue.get("key")
                # A viewed ticket is listed in both sections; keep its first (History) entry.
                if not key or key in seen:
                    continue
                seen.add(key)
                img = issue.get("img") or None
                results.append(JiraIssueSummary(
                    key=key,
                    summary=issue.get("summaryText") or issue.get("summary") or "",
                    # Jira sometimes returns this relative to the instance root.
                    icon_url=str(httpx.URL(self._base_url).join(img)) if img else None,
                    section=section_label,
                ))
                if len(results) >= max_results:
                    return results
        return results

    async def fetch_myself(self) -> str:
        """The authenticated account's display name; proves the credentials work."""
        headers, auth = self._auth()
        # Cloud has v3; Server/Data Center (PAT, no email) only has v2.
        version = 3 if self._email else 2
        async with self._client(10.0) as client:
            try:
                response = await client.get(
                    f"{self._base_url}/rest/api/{version}/myself",
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
        self._raise_for_status(response, "sign-in check")
        try:
            account = response.json()
        except ValueError as exc:
            raise JiraError("Jira returned an unreadable account") from exc
        if not isinstance(account, dict):
            raise JiraError("Jira returned an unreadable account")
        return account.get("displayName") or account.get("emailAddress") or account.get("name") or "unknown account"

    async def fetch_issue(self, issue_key: str) -> dict:
        headers, auth = self._auth()
        async with self._client(15.0) as client:
            try:
                response = await client.get(
                    f"{self._base_url}/rest/api/3/issue/{issue_key}",
                    params={"fields": "summary,description,attachment,updated", "expand": "renderedFields"},
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
        self._raise_for_status(response, f"fetch {issue_key}", not_found_key=issue_key)
        return response.json()

    async def fetch_comments(self, issue_key: str) -> list[dict]:
        """All comments, oldest first. Paged explicitly: the `comment` field embedded in the
        issue response can be truncated on busy tickets."""
        headers, auth = self._auth()
        comments: list[dict] = []
        async with self._client(15.0) as client:
            for _ in range(50):  # 5000 comments; a guard against a server that never ends paging
                try:
                    response = await client.get(
                        f"{self._base_url}/rest/api/3/issue/{issue_key}/comment",
                        params={"expand": "renderedBody", "startAt": len(comments), "maxResults": 100},
                        headers={"Accept": "application/json", **headers},
                        auth=auth,
                    )
                except httpx.HTTPError as exc:
                    raise JiraError(f"Cannot reach Jira: {exc}") from exc
                self._raise_for_status(response, f"fetch comments for {issue_key}", not_found_key=issue_key)
                try:
                    page = response.json()
                except ValueError as exc:
                    raise JiraError("Jira returned an unreadable comment page") from exc
                batch = page.get("comments") or []
                comments.extend(batch)
                if not batch or len(comments) >= int(page.get("total") or 0):
                    break
        return comments

    async def sync_ticket(self, issue_key: str, context_dir: Path, *, at_task_creation: bool = False) -> TicketSync:
        """Bring context_dir up to date with the ticket: attachments/, attachments-removed/,
        frames/, jira-issue.md and jira-manifest.json.

        Runs at task creation (no keyframes yet: ffmpeg is slow, and the Architect's first run
        does it) and before every Architect run. Each sync bumps the manifest's syncGeneration;
        mark_prompted records which generation a launched Architect was actually shown, so
        prompts can say what changed "since your last run" even when a launch was refused.

        Incremental and idempotent: files already on disk at their Jira size are not downloaded
        again. A problem with one attachment (too big, over the count or total limit, a failed or
        untrusted download, a bad video) skips just that file and is recorded; only a whole-ticket
        failure (auth, 404, network) raises. Every write lands via a temp file and a rename, so
        two overlapping syncs of one task waste work but never leave a truncated file behind.

        Filenames are prefixed with the attachment id and sanitized: Jira allows duplicate
        filenames, and the name itself is user-supplied at upload time.
        """
        issue = await self.fetch_issue(issue_key)
        fields = issue.get("fields") or {}
        result = TicketSync(title=fields.get("summary") or None)
        comments: list[dict] = []
        if self._config.fetch_comments:
            try:
                comments = await self.fetch_comments(issue_key)
            except JiraError as exc:
                result.comments_error = str(exc)

        attachments_dir = context_dir / "attachments"
        removed_dir = context_dir / "attachments-removed"
        attachments_dir.mkdir(parents=True, exist_ok=True)
        manifest_path = context_dir / "jira-manifest.json"
        previous = _read_manifest(manifest_path)
        entries: dict[str, dict] = {e["id"]: e for e in previous.get("attachments", []) if e.get("id")}

        local: dict[str, Path] = {}
        for path in attachments_dir.iterdir():
            if path.name.endswith(".part"):
                _unlink_quietly(path)  # left behind by a sync that was killed mid-download
            elif path.is_file() and (match := re.match(r"([0-9]{1,30})-", path.name)):
                local[match.group(1)] = path

        generation = int(previous.get("syncGeneration") or 0) + 1
        last_prompted = previous.get("lastPromptedGeneration")
        # Files on disk but no manifest: a task synced before manifests existed, whose Architect
        # has most likely already read them. Count them (and that run) as generation 0, so this
        # sync's additions and removals are reported as changes rather than silently absorbed.
        legacy = not previous and bool(local)
        if legacy:
            last_prompted = 0

        rendered = issue.get("renderedFields") or {}
        texts = {"description": rendered.get("description") or ""}
        texts.update({f"comment:{c.get('id')}": c.get("renderedBody") or "" for c in comments})
        by_name = {a.get("filename"): str(a.get("id")) for a in fields.get("attachment") or []}
        references: dict[str, list[str]] = {}
        for where, text in texts.items():
            for attachment_id in _referenced_ids(text, by_name):
                references.setdefault(attachment_id, []).append(where)

        on_ticket: set[str] = set()
        ticket_ids = {str(a.get("id")) for a in fields.get("attachment") or []}
        total = sum(p.stat().st_size for i, p in local.items() if i in ticket_ids)
        async with self._client(60.0) as client:
            for index, attachment in enumerate(fields.get("attachment") or []):
                attachment_id = str(attachment.get("id", ""))
                filename = str(attachment.get("filename") or "attachment")
                if not re.fullmatch(r"[0-9]{1,30}", attachment_id):
                    result.skipped.append(SkippedAttachment("?", _sanitize_filename(filename), "invalid attachment ID"))
                    continue
                on_ticket.add(attachment_id)
                name = _sanitize_filename(filename)
                size = attachment.get("size") if isinstance(attachment.get("size"), int) else None
                seen = 0 if legacy and attachment_id in local else generation
                entry = entries.setdefault(attachment_id, {"id": attachment_id, "firstSeenGeneration": seen})
                entry.update(file=f"{attachment_id}-{name}", filename=filename, mimeType=attachment.get("mimeType"),
                             size=size, referencedIn=references.get(attachment_id, []), reason=None)
                existing = local.get(attachment_id)
                if existing and (size is None or existing.stat().st_size == size):
                    entry.update(status="present", file=existing.name)
                    result.kept.append(existing.name)
                    continue
                if existing:  # truncated or replaced; download it again
                    total -= existing.stat().st_size
                    _unlink_quietly(existing)
                reason = self._skip_reason(index, size, total, attachment.get("content"))
                dest = attachments_dir / entry["file"]
                if reason is None and dest.resolve().parent != attachments_dir.resolve():
                    reason = "unsafe file name"
                if reason is None:
                    reason = await self._download_one(client, issue_key, attachment_id, attachment["content"], dest,
                                                      self._config.max_total_attachment_bytes - total)
                if reason is not None:
                    entry.update(status="skipped", reason=reason)
                    result.skipped.append(SkippedAttachment(attachment_id, name, reason))
                    continue
                total += dest.stat().st_size
                entry["status"] = "present"
                result.added.append(dest.name)

        for attachment_id, path in local.items():
            if attachment_id in on_ticket:
                continue
            removed_dir.mkdir(exist_ok=True)
            try:
                os.replace(path, removed_dir / path.name)
            except OSError:
                continue
            result.removed.append(path.name)
            entries.setdefault(attachment_id, {"id": attachment_id, "file": path.name, "filename": path.name,
                                               "firstSeenGeneration": 0 if legacy else generation})
        for attachment_id, entry in entries.items():
            if attachment_id not in on_ticket and entry.get("status") != "removed":
                entry.update(status="removed", removedGeneration=generation, referencedIn=[], reason=None)

        if not at_task_creation and self._config.max_frames_per_video > 0:
            await self._sync_frames(context_dir, entries, result)

        manifest = {
            "issueKey": issue_key,
            "syncedAt": datetime.now(timezone.utc).isoformat(),
            "ticketUpdated": fields.get("updated"),
            "syncGeneration": generation,
            "lastPromptedGeneration": last_prompted,
            "commentsError": result.comments_error,
            "attachments": sorted(entries.values(), key=lambda e: int(e["id"])),
        }
        comments_section = comments if self._config.fetch_comments else None
        _write_atomic(context_dir / "jira-issue.md",
                      _issue_markdown(issue_key, fields, rendered, comments_section, entries, result.comments_error))
        _write_atomic(manifest_path, json.dumps(manifest, indent=2) + "\n")
        return result

    def _skip_reason(self, index: int, size: int | None, total: int, content_url: str | None) -> str | None:
        cfg = self._config
        if not content_url:
            return "Jira gave no download URL"
        if index >= cfg.max_attachments:
            return f"over the maxAttachments limit ({cfg.max_attachments})"
        if size is not None and size > cfg.max_attachment_bytes:
            return f"{_mb(size)} is over maxAttachmentBytes ({_mb(cfg.max_attachment_bytes)})"
        if size is not None and total + size > cfg.max_total_attachment_bytes:
            return f"would exceed maxTotalAttachmentBytes ({_mb(cfg.max_total_attachment_bytes)})"
        return None

    async def _download_one(self, client, issue_key: str, attachment_id: str, url: str, dest: Path,
                            total_left: int) -> str | None:
        """Download to dest via a .part file. Returns None on success, else the skip reason."""
        part = dest.with_name(dest.name + ".part")
        try:
            await self._download(client, url, part, min(self._config.max_attachment_bytes, total_left))
            os.replace(part, dest)
            return None
        except (JiraError, httpx.HTTPError, OSError) as exc:
            _unlink_quietly(part)
            event('jira.attachment.download_failed', issueKey=issue_key, attachmentId=attachment_id)
            if dest.is_file():  # an overlapping sync of the same task finished it first
                return None
            return f"download failed: {exc}"

    async def _sync_frames(self, context_dir: Path, entries: dict[str, dict], result: TicketSync) -> None:
        """Keyframes for new videos, within one time budget for the whole sync: it runs before an
        Architect launch (for final verification, inside the last reviewer's flowctl call), so it
        must stay bounded however many videos the ticket has. Videos left over are picked up by
        the next sync."""
        cfg = self._config
        budget = cfg.frame_extraction_timeout_seconds
        deadline = time.monotonic() + budget
        for entry in entries.values():
            if entry.get("status") != "present" or not media_frames.is_video(entry["file"], entry.get("mimeType")):
                continue
            dest = context_dir / "frames" / entry["id"]
            done = media_frames.frames_done(dest)
            if done or entry.get("framesChecked"):
                entry["frames"] = done or None
                continue
            remaining = deadline - time.monotonic()
            if remaining < 1:
                reason = "deferred to the next sync (frame-extraction time budget used up)"
                entry.update(frames=None, framesError=reason)
                result.frame_errors[entry["id"]] = reason
                continue
            try:
                names = await media_frames.extract_frames(
                    context_dir / "attachments" / entry["file"], dest, max_frames=cfg.max_frames_per_video,
                    timeout=remaining, ffmpeg_path=cfg.ffmpeg_path)
            except (media_frames.FrameExtractionError, OSError) as exc:
                # Don't retry a video that failed to decode, or timed out with the whole budget to
                # itself: it would just fail (slowly) again on every run. Do retry once ffmpeg is
                # installed, or when it only ran out of budget left over by earlier videos.
                cut_short = "timed out" in str(exc) and remaining < budget - 1
                entry.update(frames=None, framesError=str(exc),
                             framesChecked=not (isinstance(exc, media_frames.ToolMissing) or cut_short))
                result.frame_errors[entry["id"]] = str(exc)
                continue
            # A static GIF yields no frames; remember that so it isn't re-probed every run.
            entry.update(frames=names or None, framesChecked=True, framesError=None)
            if names:
                result.frames[entry["id"]] = len(names)

    async def transition_to(self, issue_key: str, target_status: str, from_statuses: list[str]) -> bool:
        """Best-effort: move issue_key to target_status, but only if its current status is
        one of from_statuses (case-insensitive, "*" = any). Returns False (not an error) when
        the guard excludes it, when it is already there, or when no transition to
        target_status is available from the current status.
        """
        return (await self.transition(issue_key, target_status, from_statuses)).moved

    async def transition(self, issue_key: str, target_status: str, from_statuses: list[str]) -> TransitionOutcome:
        """`transition_to`, saying why the ticket did not move."""
        headers, auth = self._auth()
        async with self._client(10.0) as client:
            try:
                response = await client.get(
                    f"{self._base_url}/rest/api/3/issue/{issue_key}",
                    params={"fields": "status"},
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
            self._raise_for_status(response, f"fetch status for {issue_key}", not_found_key=issue_key)
            current = ((response.json().get("fields") or {}).get("status") or {}).get("name", "")
            if current.casefold() == target_status.casefold():
                return TransitionOutcome("already", current)
            if not status_allowed(current, from_statuses):
                return TransitionOutcome("not-allowed", current)

            try:
                response = await client.get(
                    f"{self._base_url}/rest/api/3/issue/{issue_key}/transitions",
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
            self._raise_for_status(response, f"list transitions for {issue_key}", not_found_key=issue_key)
            match = next(
                (t for t in response.json().get("transitions", [])
                 if (t.get("to") or {}).get("name", "").casefold() == target_status.casefold()),
                None,
            )
            if match is None:
                return TransitionOutcome("unavailable", current)

            try:
                response = await client.post(
                    f"{self._base_url}/rest/api/3/issue/{issue_key}/transitions",
                    json={"transition": {"id": match["id"]}},
                    headers={"Accept": "application/json", **headers},
                    auth=auth,
                )
            except httpx.HTTPError as exc:
                raise JiraError(f"Cannot reach Jira: {exc}") from exc
        self._raise_for_status(response, f"transition {issue_key} to {target_status}", not_found_key=issue_key)
        return TransitionOutcome("moved", current)

    @staticmethod
    def _raise_for_status(response: httpx.Response, action: str, not_found_key: str | None = None) -> None:
        if response.status_code == 401 or response.status_code == 403:
            raise JiraError("Jira rejected the configured credentials")
        if response.status_code == 404 and not_found_key:
            raise JiraError(f"Jira issue {not_found_key} not found")
        if response.status_code >= 300:
            raise JiraError(f"Jira {action} failed ({response.status_code})")


_FILENAME_SAFE = re.compile(r"[^A-Za-z0-9 ._-]+")


def _sanitize_filename(name: str) -> str:
    name = Path(name).name  # drop any path components the uploaded name might carry
    name = _FILENAME_SAFE.sub("_", name).strip(" ._") or "attachment"
    return name[:200]


_TAG_RE = re.compile(r"<[^>]+>")
_BLOCK_CLOSE_RE = re.compile(r"</(p|li|h[1-6]|div|ul|ol)>", re.IGNORECASE)
_BR_RE = re.compile(r"<br\s*/?>", re.IGNORECASE)


# Ways rendered Jira HTML points at one of the ticket's own attachments: Cloud's REST content and
# thumbnail URLs, Server/DC's /secure/ paths, and media-embed data attributes.
_ATTACHMENT_ID_RES = [re.compile(p, re.IGNORECASE) for p in (
    r"/attachment/content/([0-9]{1,30})",
    r"/attachment/thumbnail/([0-9]{1,30})",
    r"/secure/attachment/([0-9]{1,30})/",
    r"/secure/thumbnail/([0-9]{1,30})/",
    r"data-attachment-id=[\"']([0-9]{1,30})[\"']",
)]
_ATTACHMENT_NAME_RE = re.compile(r"(?:data-attachment-name|alt)=[\"']([^\"']+)[\"']", re.IGNORECASE)
_ANCHOR_RE = re.compile(r"<a\b[^>]*>.*?</a>", re.IGNORECASE | re.DOTALL)
_IMAGE_EXTENSIONS = frozenset({".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"})


def _unescape(rendered_html: str) -> str:
    return html.unescape(html.unescape(rendered_html))


def _attachment_id(markup: str, by_name: dict[str, str]) -> str | None:
    for pattern in _ATTACHMENT_ID_RES:
        if match := pattern.search(markup):
            return match.group(1)
    if match := _ATTACHMENT_NAME_RE.search(markup):
        return by_name.get(match.group(1))
    return None


def _referenced_ids(rendered_html: str, by_name: dict[str, str]) -> list[str]:
    ids: list[str] = []
    for tag in _TAG_RE.findall(_unescape(rendered_html)):
        attachment_id = _attachment_id(tag, by_name)
        if attachment_id and attachment_id not in ids:
            ids.append(attachment_id)
    return ids


def _marker(attachment_id: str, entries: dict[str, dict]) -> str:
    """Inline stand-in for an embedded attachment, pointing at its local copy (paths are
    relative to the context directory holding jira-issue.md)."""
    entry = entries.get(attachment_id)
    if not entry or entry.get("status") == "removed":
        return f"[attachment {attachment_id}: not an attachment of this ticket]"
    if entry.get("status") != "present":
        return f"[attachment {attachment_id} ({entry.get('filename')}): not downloaded - {entry.get('reason')}]"
    path = f"attachments/{entry['file']}"
    if media_frames.is_video(entry["file"], entry.get("mimeType")) and entry.get("frames"):
        return f"[video: {path} - keyframes in frames/{attachment_id}/]"
    if media_frames.is_video(entry["file"], entry.get("mimeType")):
        return f"[video: {path}]"
    if (entry.get("mimeType") or "").startswith("image/") or Path(entry["file"]).suffix.lower() in _IMAGE_EXTENSIONS:
        return f"[image: {path}]"
    return f"[attachment: {path}]"


def _html_to_text(rendered_html: str, entries: dict[str, dict] | None = None,
                  by_name: dict[str, str] | None = None) -> str:
    """Good-enough HTML->text for Jira's renderedFields (entity-escaped tags). Embedded
    attachments become inline markers instead of vanishing with the rest of the markup."""
    entries, by_name = entries or {}, by_name or {}

    def anchor(match: re.Match) -> str:
        attachment_id = _attachment_id(match.group(0), by_name)
        return _marker(attachment_id, entries) if attachment_id else match.group(0)

    def tag(match: re.Match) -> str:
        attachment_id = _attachment_id(match.group(0), by_name)
        if attachment_id:
            return _marker(attachment_id, entries)
        if match.group(0).lower().startswith("<img") and (src := re.search(r"src=[\"']([^\"']+)", match.group(0))):
            return f"[image not attached to the ticket: {src.group(1)}]"
        return match.group(0)

    text = _unescape(rendered_html)
    text = _ANCHOR_RE.sub(anchor, text)
    text = _TAG_RE.sub(tag, text)
    text = _BR_RE.sub("\n", text)
    text = _BLOCK_CLOSE_RE.sub("\n", text)
    text = _TAG_RE.sub("", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def _issue_markdown(issue_key: str, fields: dict, rendered: dict, comments: list[dict] | None = None,
                    entries: dict[str, dict] | None = None, comments_error: str | None = None) -> str:
    by_name = {a.get("filename"): str(a.get("id")) for a in fields.get("attachment") or []}
    summary = fields.get("summary", "")
    description = _html_to_text(rendered.get("description") or "", entries, by_name) or "(no description)"
    parts = [f"# {issue_key}: {summary}", "## Description", description]
    if comments_error:
        parts += ["## Comments", f"Not fetched ({comments_error}) - read them through your Jira MCP."]
    elif comments is not None:
        parts.append(f"## Comments ({len(comments)})")
        for comment in comments:
            author = (comment.get("author") or {}).get("displayName") or "unknown"
            created = str(comment.get("created") or "")[:16].replace("T", " ")
            body = _html_to_text(comment.get("renderedBody") or "", entries, by_name) or "(empty)"
            parts += [f"### Comment {comment.get('id')} - {author}, {created}", body]
        if not comments:
            parts.append("(none)")
    return "\n\n".join(parts) + "\n"


def manifest_generation(context_dir: Path) -> int | None:
    """The sync generation an Architect prompt is being built from, or None before any sync."""
    generation = _read_manifest(context_dir / "jira-manifest.json").get("syncGeneration")
    return generation if isinstance(generation, int) else None


def mark_prompted(context_dir: Path, generation: int) -> None:
    """Record that a launched Architect was shown this sync generation. Best-effort: a missed
    mark only means the next prompt re-flags some changes as new."""
    path = context_dir / "jira-manifest.json"
    manifest = _read_manifest(path)
    if not manifest:
        return
    manifest["lastPromptedGeneration"] = max(generation, manifest.get("lastPromptedGeneration") or 0)
    try:
        _write_atomic(path, json.dumps(manifest, indent=2) + "\n")
    except OSError:
        pass


def _mb(size: int) -> str:
    return f"{size / 1_000_000:.1f} MB"


def _unlink_quietly(path: Path) -> None:
    try:
        path.unlink(missing_ok=True)
    except OSError:
        pass  # e.g. still open by an overlapping sync on Windows; the next sync cleans it up


def _read_manifest(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def _write_atomic(path: Path, text: str) -> None:
    tmp = path.with_name(f"{path.name}.{secrets.token_hex(4)}.tmp")
    tmp.write_text(text, encoding="utf-8")
    os.replace(tmp, path)
