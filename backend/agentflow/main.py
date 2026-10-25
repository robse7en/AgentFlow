"""FastAPI application entry point.

Run with:  agentflow            (console script)
      or:  python -m agentflow.main
      or:  uvicorn --factory agentflow.main:create_app --host 127.0.0.1 --port 8765
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from uuid import uuid4
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse, Response
from fastapi.middleware.trustedhost import TrustedHostMiddleware
from fastapi.staticfiles import StaticFiles
from starlette.websockets import WebSocketState

from . import __version__
from .api import config as config_api
from .api import harnesses as harnesses_api
from .api import jira as jira_api
from .api import projects as projects_api
from .api import questions as questions_api
from .api import instructions as instructions_api
from .api import tasks as tasks_api
from .api import workflow as workflow_api
from .api.deps import AppState, WorkflowHTTPException
from .api.security import COOKIE, HumanAccess, allowed_origin, authorize_agent
from .config import SKIP_JIRA_CHECK, Config, ConfigError, config_path, jira_startup_problem, load_config
from .database.database import Database
from . import diagnostics
from .harnesses.registry import HarnessRegistry
from .services.chat import start_chat
from .services.events import Broadcaster
from .services.git_workspace import GitService
from .services.zellij import ZellijService
from .workflow.engine import WorkflowEngine
from .workflow.pr_sync import PrSync

log = logging.getLogger("agentflow")


async def build_state(config: Config, config_file: Path, broadcaster: Broadcaster, existing_db: Database | None = None) -> AppState:
    db = existing_db or Database(config.database_file)
    db.create_all()
    git = GitService()
    zellij = ZellijService(config.zellij)
    harnesses = HarnessRegistry(config)
    await harnesses.detect_all()
    zellij_status = {"available": zellij.executable() is not None, "version": await zellij.version(), "command": config.zellij.command}
    system = {
        "git": {"available": git.executable() is not None, "version": await git.version()},
        # Where agents run; always zellij here (the Rust backend can also embed terminals).
        "terminal": {"backend": "zellij", "available": zellij_status["available"], "version": zellij_status["version"]},
        "zellij": zellij_status,
    }
    host = f"[{config.server.host}]" if ":" in config.server.host else config.server.host
    api_base = f"http://{host}:{config.server.port}"
    engine = WorkflowEngine(config, db, git, zellij, harnesses, broadcaster, api_base)
    for key, caps in harnesses.capabilities.items():
        log.info("harness %-10s %s", key, "available" if caps.available else f"unavailable ({caps.message})")
    log.info("git: %s | zellij: %s", system["git"]["version"], system["zellij"]["version"] or "unavailable")
    return AppState(config, config_file, db, git, zellij, harnesses, broadcaster, engine, system)


def create_app(config: Config | None = None, config_file: Path | None = None) -> FastAPI:
    config_file = config_file or config_path()
    if config is None:
        # The uvicorn factory path (--reload, or `uvicorn --factory`), which bypasses run().
        config = load_config(config_file)
        if problem := jira_startup_problem(config):
            raise ConfigError(problem)
    if config.server.host not in ("127.0.0.1", "localhost", "::1"):
        raise ValueError("AgentFlow must bind to a loopback address")

    broadcaster = Broadcaster()

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        handler = diagnostics.open_log(app.state.diagnostic_path)
        chat = None
        pr_sync = None
        try:
            diagnostics.event('application.starting', version=__version__)
            app.state.agentflow = await build_state(config, config_file, broadcaster)
            await app.state.agentflow.engine.recover()
            # After recover() so Slack's catch-up sees interrupted runs; connects in the background.
            chat = start_chat(config, app.state.agentflow.engine, config_file)
            pr_sync = PrSync(app.state.agentflow.engine)
            pr_sync.start()
            code = app.state.human.pairing_code
            print(f"AgentFlow browser pairing code: {code}", flush=True)
            print(f"Open {ui_base}/#pair={code} to pair this browser.", flush=True)
            yield
        except Exception:
            diagnostics.log.exception('application.failed')
            raise
        finally:
            if pr_sync is not None:
                await pr_sync.stop()
            if chat is not None:
                await chat.stop()
            if hasattr(app.state, 'agentflow'):
                await app.state.agentflow.engine.shutdown()
                app.state.agentflow.db.engine.dispose()
            diagnostics.event('application.stopped')
            diagnostics.log.removeHandler(handler)
            handler.close()

    app = FastAPI(title="AgentFlow", version=__version__, lifespan=lifespan)
    app.state.human = HumanAccess(config.data_dir / "browser-session")
    server_host = f"[{config.server.host}]" if ":" in config.server.host else config.server.host
    dist = Path(__file__).resolve().parents[2] / "frontend" / "dist"
    # The backend serves the built UI itself; otherwise the Vite dev server does.
    ui_base = f"http://{server_host}:{config.server.port}" if dist.is_dir() else "http://127.0.0.1:5173"

    @app.exception_handler(WorkflowHTTPException)
    async def workflow_http_error(_request: Request, exc: WorkflowHTTPException):
        body = {"detail": exc.detail}
        if exc.code:
            body["code"] = exc.code
        return JSONResponse(body, status_code=exc.status_code)
    app.state.diagnostic_path = config_file.resolve().parent / '.agentflow' / 'logs' / 'agentflow.jsonl'
    app.add_middleware(TrustedHostMiddleware, allowed_hosts=["localhost", "127.0.0.1", "[::1]"])

    @app.middleware("http")
    async def permissions(request: Request, call_next):
        path = request.url.path
        if path.startswith("/api/") and request.method != "OPTIONS" and path != "/api/health":
            try:
                if path == "/api/auth/pair":
                    if not allowed_origin(request.headers.get("origin"), request.headers.get("host", "")):
                        raise HTTPException(403, "Use the local browser UI")
                elif "/agent/" in path or request.headers.get("authorization"):
                    parts = path.strip("/").split("/")
                    if len(parts) not in (3, 5) or parts[:2] != ["api", "tasks"]:
                        raise HTTPException(403, "Agents cannot perform human actions")
                    if not ((len(parts) == 3 and request.method == "GET") or (
                        len(parts) == 5 and parts[3] == "agent" and request.method == "POST"
                    )):
                        raise HTTPException(403, "Agents cannot perform human actions")
                    await authorize_agent(request, parts[2])
                else:
                    if not app.state.human.authenticated(request.cookies):
                        raise HTTPException(401, "Pair this browser using the code in the backend terminal")
                    if request.method not in ("GET", "HEAD") and not allowed_origin(
                        request.headers.get("origin"), request.headers.get("host", "")
                    ):
                        raise HTTPException(403, "Human actions require the local browser UI")
            except HTTPException as exc:
                return JSONResponse({"detail": exc.detail}, status_code=exc.status_code)
        return await call_next(request)

    @app.post("/api/auth/pair")
    async def pair(request: Request):
        try:
            body = await request.json()
        except ValueError as exc:
            raise HTTPException(422, "Expected a pairing code") from exc
        if not isinstance(body, dict) or not isinstance(body.get("code"), str):
            raise HTTPException(422, "Expected a pairing code")
        try:
            token = app.state.human.pair(body["code"])
        finally:
            # Sockets opened with the replaced session stop receiving task data now; their
            # HTTP requests are already refused. A rejected code changes nothing to revoke.
            await broadcaster.revoke()
        response = JSONResponse({"ok": True})
        response.set_cookie(COOKIE, token, max_age=400 * 24 * 3600, httponly=True, samesite="strict")
        return response

    @app.get("/api/auth/session")
    async def human_session():
        return {"ok": True}
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["http://127.0.0.1:5173", "http://localhost:5173"],
        allow_methods=["*"],
        allow_headers=["*"],
    )
    app.include_router(tasks_api.router)
    app.include_router(instructions_api.router)
    app.include_router(workflow_api.router)
    app.include_router(config_api.router)
    app.include_router(harnesses_api.router)
    app.include_router(jira_api.router)
    app.include_router(projects_api.router)
    app.include_router(questions_api.router)

    @app.middleware('http')
    async def diagnostic_requests(request: Request, call_next):
        reference = str(uuid4())
        token = diagnostics.request_id.set(reference)
        started = time.monotonic()
        try:
            try:
                response = await call_next(request)
            except Exception:
                diagnostics.log.exception('request.failed')
                response = JSONResponse(
                    {'detail': f'Internal error. Diagnostic reference: {reference}'}, status_code=500
                )
            route = request.scope.get('route')
            diagnostics.event('request.completed', method=request.method,
                              route=getattr(route, 'path', 'unmatched'), status=response.status_code,
                              durationMs=round((time.monotonic() - started) * 1000))
            response.headers['X-Request-ID'] = reference
            return response
        finally:
            diagnostics.request_id.reset(token)

    @app.get('/api/diagnostics')
    def diagnostic_log():
        # The normal pairing middleware protects this endpoint; agent tokens remain task-scoped.
        # A plain FileResponse stamps Content-Length from stat() before streaming, but the
        # diagnostic_requests middleware appends this very request's own log line to the same
        # file first - by the time the body streams, the file is longer than the header
        # promised and uvicorn raises "Response content longer than Content-Length". Reading
        # the bytes up front fixes the length at send time.
        path = app.state.diagnostic_path
        body = path.read_bytes() if path.exists() else b''
        return Response(
            body, media_type='application/x-ndjson',
            headers={'Content-Disposition': 'attachment; filename="agentflow.jsonl"'},
        )

    @app.get("/api/health")
    def health():
        return {"ok": True, "version": __version__}

    @app.websocket("/api/ws")
    async def websocket_endpoint(websocket: WebSocket):
        human = app.state.human
        # Read together with the cookie check (no await between): the session generation
        # that authorizes this socket for as long as it stays open.
        generation = human.generation
        if not human.authenticated(websocket.cookies) or not allowed_origin(
            websocket.headers.get("origin"), websocket.headers.get("host", "")
        ):
            await websocket.close(code=1008)
            return
        if not await broadcaster.connect(websocket, lambda: human.generation == generation):
            return
        try:
            # Until the client leaves, or the server closed the socket because pairing replaced its session.
            while websocket.application_state == WebSocketState.CONNECTED:
                await websocket.receive_text()  # keep-alive pings from the client
        except WebSocketDisconnect:
            pass
        finally:
            await broadcaster.disconnect(websocket)

    # Serve the built Vue frontend when present (frontend/dist), so one process is enough.
    if dist.is_dir():
        app.mount("/assets", StaticFiles(directory=dist / "assets"), name="assets")

        @app.get("/{full_path:path}")
        def spa(full_path: str):
            candidate = (dist / full_path).resolve()
            if not candidate.is_relative_to(dist.resolve()):
                raise HTTPException(404, "Not found")
            if full_path and candidate.is_file():
                return FileResponse(candidate)
            return FileResponse(dist / "index.html")

    return app


def run(argv: list[str] | None = None) -> None:
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["doctor"]:
        from .doctor import main as doctor

        sys.exit(doctor(argv[1:]))
    if argv[:1] == ["setup"]:
        from .setup_wizard import main as setup

        sys.exit(setup(argv[1:]))
    parser = argparse.ArgumentParser(prog="agentflow", description="Start the AgentFlow backend",
                                     epilog="Run `agentflow setup` to configure AgentFlow, or `agentflow doctor` to check it.")
    parser.add_argument(
        "--config",
        help=(
            "Path to config.json (defaults to config.json in the AgentFlow folder, or an existing "
            "~/.agentflow/config.json while that has none)"
        ),
    )
    parser.add_argument("--host", help="Override server.host")
    parser.add_argument("--port", type=int, help="Override server.port")
    parser.add_argument("--reload", action="store_true", help="Auto-reload on code changes (development)")
    parser.add_argument("--skip-jira-check", action="store_true",
                        help=f"Start without Jira configured (development and tests; same as {SKIP_JIRA_CHECK}=1)")
    args = parser.parse_args(argv)
    if args.skip_jira_check:
        # Through the environment, so the --reload worker and in-app config reloads skip it too.
        os.environ[SKIP_JIRA_CHECK] = "1"

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)-7s %(name)s: %(message)s")
    file = Path(args.config).expanduser() if args.config else config_path()
    try:
        config = load_config(file)
    except ConfigError as exc:
        print(f"Configuration error:\n{exc}", file=sys.stderr)
        sys.exit(2)
    if problem := jira_startup_problem(config):
        print(f"{problem}\nConfig file: {file}", file=sys.stderr)
        sys.exit(2)
    host = args.host or config.server.host
    port = args.port or config.server.port
    if args.host or args.port:
        config.server.host, config.server.port = host, port

    import uvicorn

    if args.reload:
        if args.config:
            # The reload worker builds the app from config_path(); make it the file checked above.
            os.environ["AGENTFLOW_CONFIG"] = str(file)
        uvicorn.run("agentflow.main:create_app", factory=True, host=host, port=port, reload=True)
    else:
        uvicorn.run(create_app(config, file), host=host, port=port)


if __name__ == "__main__":
    run()
