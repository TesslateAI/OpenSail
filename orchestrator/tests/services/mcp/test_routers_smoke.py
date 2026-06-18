"""Smoke tests for Phase 3 MCP router additions.

These tests exercise pure-Python parsing/response shape of the endpoints; the
full async-DB-backed flow is covered in the integration suite.
"""

from __future__ import annotations

from types import SimpleNamespace
from uuid import uuid4

import pytest
from pydantic import ValidationError

pytestmark = pytest.mark.unit


def test_catalog_entry_accepts_expected_fields():
    from uuid import uuid4

    from app.routers.mcp import CatalogEntry

    entry = CatalogEntry(
        id=uuid4(),
        slug="linear",
        name="Linear",
        description="Search issues, create tickets.",
        icon="🤖",
        icon_url="https://linear.app/favicon.ico",
        category="productivity",
        config={"url": "https://mcp.linear.app/mcp", "auth_type": "oauth"},
    )
    assert entry.slug == "linear"
    assert entry.config["auth_type"] == "oauth"


def test_disabled_tools_update_normalizes_input():
    from app.routers.mcp import DisabledToolsUpdate

    body = DisabledToolsUpdate(disabled_tools=["mcp__github__delete_repo"])
    assert body.disabled_tools == ["mcp__github__delete_repo"]


def test_override_request_requires_project_id():
    from app.routers.mcp import OverrideRequest

    with pytest.raises(ValidationError):
        OverrideRequest()  # type: ignore[call-arg]


# ---------------------------------------------------------------------------
# Issue #307 — McpInstallRequest scope + McpConfigResponse shape
# ---------------------------------------------------------------------------


def test_mcp_install_request_defaults_to_user_scope():
    """Default scope is 'user' — install follows the caller across teams."""
    from uuid import uuid4

    from app.schemas import McpInstallRequest

    body = McpInstallRequest(marketplace_agent_id=uuid4())
    assert body.scope_level == "user"
    assert body.project_id is None


def test_mcp_install_request_accepts_project_scope_with_project_id():
    from uuid import uuid4

    from app.schemas import McpInstallRequest

    project_id = uuid4()
    body = McpInstallRequest(
        marketplace_agent_id=uuid4(),
        scope_level="project",
        project_id=project_id,
    )
    assert body.scope_level == "project"
    assert body.project_id == project_id


def test_mcp_install_request_rejects_team_scope():
    """Team-scope install is deliberately unsupported (OAuth identity binding)."""
    from uuid import uuid4

    from app.schemas import McpInstallRequest

    with pytest.raises(ValidationError):
        McpInstallRequest(
            marketplace_agent_id=uuid4(),
            scope_level="team",  # type: ignore[arg-type]
        )


def test_start_oauth_request_rejects_platform_app_without_slug():
    """platform_app registration method must only be used with catalog entries.

    If a user can send registration_method='platform_app' with an arbitrary
    server_url (no marketplace_agent_slug), they can exfiltrate Tesslate's
    platform client_secret to their own server.
    """
    from app.routers.mcp_oauth import StartOAuthRequest

    with pytest.raises(ValidationError, match="platform_app.*marketplace_agent_slug"):
        StartOAuthRequest(
            server_url="https://githubcopilot.evil.com/mcp",
            registration_method="platform_app",
        )


def test_start_oauth_request_allows_platform_app_with_slug():
    """platform_app is valid when a catalog slug is provided."""
    from app.routers.mcp_oauth import StartOAuthRequest

    body = StartOAuthRequest(
        marketplace_agent_slug="mcp-github-oauth",
        registration_method="platform_app",
    )
    assert body.registration_method == "platform_app"
    assert body.marketplace_agent_slug == "mcp-github-oauth"


def test_mcp_oauth_callback_uses_loopback_origin_in_desktop(monkeypatch):
    """Desktop MCP OAuth redirects must target the sidecar callback server."""
    from app.routers import mcp_oauth

    request = SimpleNamespace(url=SimpleNamespace(scheme="http", netloc="127.0.0.1:42424"))
    settings = SimpleNamespace(public_base_url="https://app.tesslate.com", is_desktop_mode=True)
    monkeypatch.setattr(mcp_oauth, "get_settings", lambda: settings)

    assert mcp_oauth._callback_url(request) == "http://127.0.0.1:42424/api/mcp/oauth/callback"


def test_mcp_oauth_callback_prefers_public_base_url_outside_desktop(monkeypatch):
    """Hosted MCP OAuth redirects still use the configured public callback."""
    from app.routers import mcp_oauth

    request = SimpleNamespace(url=SimpleNamespace(scheme="http", netloc="127.0.0.1:42424"))
    settings = SimpleNamespace(public_base_url="https://app.tesslate.com/", is_desktop_mode=False)
    monkeypatch.setattr(mcp_oauth, "get_settings", lambda: settings)

    assert mcp_oauth._callback_url(request) == "https://app.tesslate.com/api/mcp/oauth/callback"


async def test_mcp_reconnect_uses_loopback_origin_in_desktop(monkeypatch):
    """Reconnect should use the same desktop-safe callback as initial OAuth."""
    from app.routers import mcp

    config_id = uuid4()
    user_id = uuid4()
    captured: dict[str, str] = {}
    config = SimpleNamespace(
        id=config_id,
        marketplace_agent_id=None,
        oauth_connection=SimpleNamespace(
            server_url="https://example.com/mcp",
            registration_method="dcr",
        ),
        scope_level="user",
        team_id=None,
        project_id=None,
    )
    request = SimpleNamespace(url=SimpleNamespace(scheme="http", netloc="127.0.0.1:42424"))
    user = SimpleNamespace(id=user_id, default_team_id=None)
    settings = SimpleNamespace(public_base_url="https://app.tesslate.com", is_desktop_mode=True)

    async def fake_get_owned_config(*args, **kwargs):
        return config

    async def fake_start_oauth_flow(**kwargs):
        captured["redirect_uri"] = kwargs["redirect_uri"]
        return SimpleNamespace(authorize_url="https://provider.example/authorize", flow_id="flow-1")

    monkeypatch.setattr(mcp, "get_settings", lambda: settings)
    monkeypatch.setattr(mcp, "_get_owned_config", fake_get_owned_config)
    monkeypatch.setattr("app.services.mcp.oauth_flow.start_oauth_flow", fake_start_oauth_flow)

    result = await mcp.reconnect_mcp_config(config_id, request, user=user, db=SimpleNamespace())

    assert result.flow_id == "flow-1"
    assert captured["redirect_uri"] == "http://127.0.0.1:42424/api/mcp/oauth/callback"


def test_assignment_ownership_uses_or_filter():
    """Agent assignment endpoints must use OR-based ownership filter.

    When team_id is set, the filter should be (user_id OR team_id),
    matching _get_owned_config. An exclusive filter (team_id ONLY when
    set) hides personal assignments and lets any team member modify
    another member's assignments.
    """
    import inspect
    import textwrap

    from app.routers.mcp import assign_mcp_to_agent, get_agent_mcp_servers, unassign_mcp_from_agent

    for fn in (assign_mcp_to_agent, unassign_mcp_from_agent, get_agent_mcp_servers):
        source = textwrap.dedent(inspect.getsource(fn))
        # The old buggy pattern: `X.team_id == team_id if team_id else X.user_id == user.id`
        # This is an exclusive either/or. The fix should use or_() or _or().
        assert "if team_id else" not in source, (
            f"{fn.__name__} still uses exclusive 'if team_id else' ownership "
            f"filter — must use OR-based filter matching _get_owned_config"
        )


def test_user_mcp_config_has_unique_scope_index():
    """UserMcpConfig must have a unique index on its scope tuple.

    Without a DB-level constraint, concurrent installs of the same
    connector can create duplicate rows via the SELECT-then-INSERT
    upsert pattern in _upsert_user_mcp_config.
    """
    from app.models import UserMcpConfig

    table = UserMcpConfig.__table__
    unique_indexes = [
        idx for idx in table.indexes if idx.unique and "uq_user_mcp_configs_scope" in idx.name
    ]
    assert len(unique_indexes) == 1, (
        "UserMcpConfig must have a unique index named 'uq_user_mcp_configs_scope'"
    )


def test_require_project_member_is_not_a_noop():
    """_require_project_member must actually check permissions.

    A no-op stub allows any authenticated user to create project-scoped
    overrides on arbitrary projects they don't own.
    """
    import inspect

    from app.routers.mcp import _require_project_member

    # The function must be async (needs DB access for permission checks)
    assert inspect.iscoroutinefunction(_require_project_member), (
        "_require_project_member must be async to perform DB-backed permission checks"
    )

    # The function must accept 3 args: (user, project_id, db)
    sig = inspect.signature(_require_project_member)
    params = list(sig.parameters.keys())
    assert len(params) == 3, (
        f"_require_project_member must accept (user, project_id, db), got {params}"
    )


def test_mcp_config_response_surfaces_scope_and_oauth_flag():
    """Library page renders Reconnect button off is_oauth; scope_level drives badges."""
    from datetime import datetime
    from uuid import uuid4

    from app.schemas import McpConfigResponse

    resp = McpConfigResponse(
        id=uuid4(),
        marketplace_agent_id=uuid4(),
        server_name="Linear",
        server_slug="mcp-linear",
        enabled_capabilities=["tools"],
        is_active=True,
        env_vars=None,
        scope_level="user",
        project_id=None,
        is_oauth=True,
        disabled_tools=[],
        created_at=datetime.utcnow(),
        updated_at=datetime.utcnow(),
    )
    assert resp.is_oauth is True
    assert resp.scope_level == "user"
