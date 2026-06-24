"""
Model Adapters

Adapters for different LLM providers that normalize their APIs to a common interface.
This allows the agent to work with ANY model without changing the core logic.

Supported:
- OpenAI API (GPT-4, GPT-3.5)
- OpenAI-compatible APIs (Cerebras, Groq, etc.)
- Anthropic API (Claude)
- Future: Ollama, HuggingFace, etc.
"""

import logging
import os
from abc import ABC, abstractmethod
from collections.abc import AsyncGenerator
from typing import Any
from uuid import UUID

from openai import AsyncOpenAI
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

logger = logging.getLogger(__name__)

# =============================================================================
# Environment variable names for DB-less standalone mode
# =============================================================================
# When create_model_adapter / get_llm_client are called without a DB session
# (e.g. from CLI tools, benchmark harnesses, or other standalone contexts),
# provider API keys are resolved from environment variables instead of the
# per-user UserAPIKey table. This mapping is the single source of truth for
# which env var holds which provider's credential.
BYOK_PROVIDER_ENV_VARS: dict[str, str] = {
    "openai": "OPENAI_API_KEY",
    "anthropic": "ANTHROPIC_API_KEY",
    "openrouter": "OPENROUTER_API_KEY",
    "requesty": "REQUESTY_API_KEY",
    "groq": "GROQ_API_KEY",
    "together": "TOGETHER_API_KEY",
    "deepseek": "DEEPSEEK_API_KEY",
    "fireworks": "FIREWORKS_API_KEY",
    "nano-gpt": "NANO_GPT_API_KEY",
    "z-ai": "Z_AI_API_KEY",
}

# Env vars for the LiteLLM proxy path (used when the model has no BYOK prefix
# or uses the explicit "builtin/" prefix).
LITELLM_API_KEY_ENV_VAR = "LITELLM_MASTER_KEY"
LITELLM_API_BASE_ENV_VAR = "LITELLM_API_BASE"


class MissingApiKeyError(ValueError):
    """
    Raised by the DB-less model adapter path when a provider's API key
    environment variable is not set.

    Subclasses :class:`ValueError` so that existing callers that catch
    ``ValueError`` (the historical error type for the DB-backed path)
    continue to work without modification, but standalone / benchmark
    callers can catch this specific type to give a precise diagnostic.

    The :attr:`env_var` attribute exposes the name of the missing env var
    so callers can surface actionable "please export X" messages without
    scraping the string.
    """

    def __init__(self, env_var: str, message: str) -> None:
        super().__init__(message)
        self.env_var = env_var


# =============================================================================
# Built-in Model Prefix
# =============================================================================
# System models served via LiteLLM are prefixed with "builtin/" in the API
# response to distinguish them from BYOK provider models that also use "/" in
# their identifiers (e.g., "openai/gpt-5.2").
BUILTIN_PREFIX = "builtin/"

# Custom provider models are prefixed with "custom/" so routing is deterministic
# and user-chosen slugs can never collide with built-in provider names.
# Format: custom/{provider_slug}/{model_id}
CUSTOM_PREFIX = "custom/"


# =============================================================================
# Built-in Provider Configurations
# =============================================================================
# Add new providers here to make them available for BYOK
# Each provider needs: name, base_url, api_type, and optionally default_headers

BUILTIN_PROVIDERS: dict[str, dict[str, Any]] = {
    "openrouter": {
        "name": "OpenRouter",
        "description": "Access to 200+ AI models through a unified API",
        "base_url": "https://openrouter.ai/api/v1",
        "api_type": "openai",
        "default_headers": {"HTTP-Referer": "https://tesslate.com", "X-Title": "OpenSail"},
        "website": "https://openrouter.ai",
        "requires_key": True,
    },
    "requesty": {
        "name": "Requesty",
        "description": "Access to 300+ AI models through a unified OpenAI-compatible API",
        "base_url": "https://router.requesty.ai/v1",
        "api_type": "openai",
        "default_headers": {"HTTP-Referer": "https://tesslate.com", "X-Title": "OpenSail"},
        "website": "https://requesty.ai",
        "requires_key": True,
    },
    "nano-gpt": {
        "name": "NanoGPT",
        "description": "Pay-per-prompt access to 200+ AI models",
        "base_url": "https://nano-gpt.com/api/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://nano-gpt.com",
        "requires_key": True,
    },
    "openai": {
        "name": "OpenAI",
        "description": "GPT-4, GPT-4o, GPT-3.5, and other OpenAI models",
        "base_url": "https://api.openai.com/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://platform.openai.com",
        "requires_key": True,
    },
    "anthropic": {
        "name": "Anthropic",
        "description": "Claude Opus 4.6, Sonnet 4.6, and other Anthropic models",
        "base_url": "https://api.anthropic.com/v1",
        "api_type": "anthropic",
        "default_headers": {},
        "website": "https://console.anthropic.com",
        "requires_key": True,
        "prompt_caching": "explicit",  # Requires cache_control annotations
    },
    "groq": {
        "name": "Groq",
        "description": "Ultra-fast inference with Llama, GPT-OSS, and more",
        "base_url": "https://api.groq.com/openai/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://console.groq.com",
        "requires_key": True,
    },
    "together": {
        "name": "Together AI",
        "description": "Open-source models with fast inference",
        "base_url": "https://api.together.xyz/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://api.together.xyz",
        "requires_key": True,
    },
    "deepseek": {
        "name": "DeepSeek",
        "description": "DeepSeek-V3.2 and other DeepSeek models",
        "base_url": "https://api.deepseek.com/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://platform.deepseek.com",
        "requires_key": True,
    },
    "fireworks": {
        "name": "Fireworks AI",
        "description": "Fast inference for open-source models",
        "base_url": "https://api.fireworks.ai/inference/v1",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://fireworks.ai",
        "requires_key": True,
    },
    "z-ai": {
        "name": "Z.AI (ZhipuAI)",
        "description": "GLM-5, GLM-4.7, and other ZhipuAI models — includes Coding Plan subscriptions",
        "base_url": "https://api.z.ai/api/paas/v4",
        "api_type": "openai",
        "default_headers": {},
        "website": "https://z.ai",
        "requires_key": True,
    },
}


def get_builtin_provider_config(provider_slug: str) -> dict[str, Any] | None:
    """Get configuration for a built-in provider."""
    return BUILTIN_PROVIDERS.get(provider_slug)


def resolve_model_name(model: str) -> str:
    """Strip routing prefixes from a model ID to get the name for the LLM API call.

    - "builtin/deepseek-v3.2" → "deepseek-v3.2"
    - "openrouter/anthropic/claude-3.5-sonnet" → "anthropic/claude-3.5-sonnet"
    - "custom/my-provider/model-x" → "model-x" (strips custom/ and provider slug)
    - "gpt-4o" → "gpt-4o" (no prefix, unchanged)
    """
    if model.startswith(BUILTIN_PREFIX):
        return model[len(BUILTIN_PREFIX) :]
    if model.startswith(CUSTOM_PREFIX):
        stripped = model[len(CUSTOM_PREFIX) :]
        # custom/{provider_slug}/{model_id} → {model_id}
        parts = stripped.split("/", 1)
        return parts[1] if len(parts) > 1 else stripped
    if "/" in model:
        provider_slug = model.split("/", 1)[0]
        if provider_slug in BUILTIN_PROVIDERS:
            return model.removeprefix(f"{provider_slug}/")
    return model


def extract_provider_slug(model_name: str) -> str | None:
    """Extract the BYOK provider slug from a model identifier, if present.

    Returns the slug only when it matches a known ``BUILTIN_PROVIDERS`` entry.
    Returns ``None`` for builtin/LiteLLM models and custom providers.

    Examples::

        "anthropic/claude-3.5-sonnet"           → "anthropic"
        "openrouter/anthropic/claude-3.5-sonnet" → "openrouter"
        "builtin/claude-opus-4.6"               → None  (LiteLLM-proxied)
        "claude-opus-4.6"                       → None  (no prefix)
        "custom/my-provider/model-x"            → None  (custom provider)
    """
    if not model_name:
        return None
    if model_name.startswith(BUILTIN_PREFIX) or model_name.startswith(CUSTOM_PREFIX):
        return None
    if "/" in model_name:
        slug = model_name.split("/", 1)[0]
        if slug in BUILTIN_PROVIDERS:
            return slug
    return None


def get_byok_provider_prefixes() -> tuple[str, ...]:
    """Return all BYOK provider prefixes derived from BUILTIN_PROVIDERS.

    This is the single source of truth for which model prefixes indicate
    BYOK (user-supplied API key) routing. Any provider added to
    BUILTIN_PROVIDERS that has requires_key=True is automatically included.
    """
    return tuple(
        f"{slug}/" for slug, cfg in BUILTIN_PROVIDERS.items() if cfg.get("requires_key", False)
    )


async def get_user_api_key(
    user_id: UUID, provider_slug: str, db: AsyncSession
) -> dict[str, str | None]:
    """
    Get user's API key and optional base URL override for a specific provider.

    Args:
        user_id: The user ID
        provider_slug: Provider identifier (e.g., "openrouter", "groq")
        db: Database session

    Returns:
        Dict with "key" (decrypted API key) and "base_url" (optional override)

    Raises:
        ValueError: If no API key configured for the provider
    """
    from ..models import UserAPIKey
    from ..routers.secrets import decode_key

    result = await db.execute(
        select(UserAPIKey).where(
            UserAPIKey.user_id == user_id,
            UserAPIKey.provider == provider_slug,
            UserAPIKey.is_active.is_(True),
        )
    )
    api_key_record = result.scalar_one_or_none()

    if not api_key_record:
        provider_name = BUILTIN_PROVIDERS.get(provider_slug, {}).get("name", provider_slug)
        raise ValueError(
            f"{provider_name} model selected but no API key configured. "
            f"Please add your {provider_name} API key in Library → Models."
        )

    return {
        "key": decode_key(api_key_record.encrypted_value),
        "base_url": api_key_record.base_url,
    }


# Bare-name routing hints for the DB-less path. When a caller provides a
# model name with no routing prefix (e.g. "gpt-4o" or "claude-3-5-sonnet"),
# we infer the provider from these prefixes before falling back to the
# LiteLLM proxy. This matches the behavior benchmark harnesses expect:
# "gpt-4o" should go direct to OpenAI when OPENAI_API_KEY is present,
# rather than silently requiring a LiteLLM proxy to be running.
_BARE_NAME_OPENAI_PREFIXES: tuple[str, ...] = ("gpt-", "o1-", "o3-", "o4-")
_BARE_NAME_ANTHROPIC_PREFIXES: tuple[str, ...] = ("claude-",)


def _build_provider_client(provider_slug: str, model_name: str) -> AsyncOpenAI:
    """
    Construct an ``AsyncOpenAI`` client for a specific built-in provider,
    reading credentials from environment variables.

    The provider's base URL is taken from :data:`BUILTIN_PROVIDERS` unless
    an override env var ``<PROVIDER>_API_BASE`` is set (e.g.
    ``OPENAI_API_BASE``), which lets benchmark harnesses point at private
    proxies without patching code.

    Raises:
        MissingApiKeyError: If the env var for ``provider_slug`` is unset.
        ValueError: If ``provider_slug`` is not a known built-in provider.
    """
    provider_config = BUILTIN_PROVIDERS.get(provider_slug)
    if not provider_config:
        raise ValueError(
            f"Unknown provider '{provider_slug}'. "
            f"Known providers: {', '.join(sorted(BUILTIN_PROVIDERS.keys()))}."
        )
    env_var = BYOK_PROVIDER_ENV_VARS.get(provider_slug)
    if not env_var:
        raise ValueError(
            f"No environment variable mapping configured for provider "
            f"'{provider_slug}'. Add an entry to BYOK_PROVIDER_ENV_VARS."
        )
    api_key = os.environ.get(env_var)
    if not api_key:
        raise MissingApiKeyError(
            env_var,
            f"{provider_config['name']} model '{model_name}' requires the "
            f"{env_var} environment variable to be set for standalone "
            f"(DB-less) operation.",
        )
    base_url = (
        os.environ.get(f"{env_var.removesuffix('_API_KEY')}_API_BASE")
        or provider_config["base_url"]
    )
    logger.info("DB-less: using %s API for model: %s", provider_config["name"], model_name)
    return AsyncOpenAI(
        api_key=api_key,
        base_url=base_url,
        default_headers=provider_config.get("default_headers", {}),
    )


def _resolve_bare_name_provider(model_name: str) -> str | None:
    """
    Infer a provider slug from a prefix-less model name.

    - ``gpt-*``, ``o1-*``, ``o3-*``, ``o4-*`` → ``"openai"``
    - ``claude-*`` → ``"anthropic"`` if ``ANTHROPIC_API_KEY`` is set,
      else ``"openrouter"`` if ``OPENROUTER_API_KEY`` is set, else
      ``None`` (fall through to LiteLLM proxy).

    Returns ``None`` when no rule matches, so the caller falls back to
    the LiteLLM proxy path. The fallback order for Claude models is
    deliberate and deterministic so tests and benchmark harnesses see
    the same routing regardless of env-var insertion order.
    """
    lowered = model_name.lower()
    if any(lowered.startswith(p) for p in _BARE_NAME_OPENAI_PREFIXES):
        return "openai"
    if any(lowered.startswith(p) for p in _BARE_NAME_ANTHROPIC_PREFIXES):
        if os.environ.get(BYOK_PROVIDER_ENV_VARS["anthropic"]):
            return "anthropic"
        if os.environ.get(BYOK_PROVIDER_ENV_VARS["openrouter"]):
            return "openrouter"
    return None


def _build_dbless_llm_client(model_name: str) -> AsyncOpenAI:
    """
    Construct an ``AsyncOpenAI`` client from environment variables for
    standalone (DB-less) use.

    Resolution rules:

    - ``custom/<slug>/<model>`` is rejected — custom providers require a
      DB lookup to resolve ``base_url`` and ``api_key``.
    - ``<provider>/<model>`` where ``<provider>`` is a known built-in
      provider routes directly to that provider's public API, keyed by
      the provider's env var from :data:`BYOK_PROVIDER_ENV_VARS`.
    - ``builtin/<model>`` routes to the LiteLLM proxy, keyed by
      ``LITELLM_MASTER_KEY`` and based at ``LITELLM_API_BASE``.
    - A bare model name with no prefix routes to the provider inferred
      by :func:`_resolve_bare_name_provider` (OpenAI for ``gpt-*``,
      Anthropic/OpenRouter for ``claude-*``). If no rule matches, it
      falls back to the LiteLLM proxy.

    Raises:
        MissingApiKeyError: If the required env var for the resolved
            provider is missing. The ``env_var`` attribute names the
            variable that must be set.
        ValueError: If the model references an unknown provider prefix
            or a ``custom/`` prefix (these are non-recoverable in the
            standalone path).
    """
    from ..config import get_settings

    settings = get_settings()

    # Custom provider prefix has no meaning without a DB lookup — there is no
    # UserProvider table to resolve the base_url / api_key from.
    if model_name.startswith(CUSTOM_PREFIX):
        raise ValueError(
            f"Custom provider models ({CUSTOM_PREFIX}...) require a database session. "
            f"Use a built-in provider prefix or the LiteLLM proxy path instead."
        )

    # builtin/ prefix → LiteLLM proxy (short-circuit so builtin/gpt-4o does
    # NOT get re-routed to OpenAI direct by the bare-name heuristic below).
    if model_name.startswith(BUILTIN_PREFIX):
        return _build_litellm_client(model_name[len(BUILTIN_PREFIX) :], settings)

    # BYOK provider prefix (e.g. "openai/gpt-4o-mini")
    if "/" in model_name:
        provider_slug = model_name.split("/", 1)[0]
        if provider_slug in BUILTIN_PROVIDERS:
            return _build_provider_client(provider_slug, model_name)
        # Unknown prefix — in DB-less mode there is no UserCustomModel fallback.
        raise ValueError(
            f"Unknown provider prefix '{provider_slug}' in model '{model_name}'. "
            f"Known providers: {', '.join(sorted(BUILTIN_PROVIDERS.keys()))}."
        )

    # Bare model name — try to infer a provider before falling back to LiteLLM.
    inferred = _resolve_bare_name_provider(model_name)
    if inferred is not None:
        logger.debug("DB-less: bare model %r inferred as provider %r", model_name, inferred)
        return _build_provider_client(inferred, model_name)

    # Fall-through: LiteLLM proxy.
    return _build_litellm_client(model_name, settings)


def _build_litellm_client(model_name: str, settings: Any) -> AsyncOpenAI:
    """
    Build an ``AsyncOpenAI`` client aimed at the LiteLLM proxy using env
    vars, falling back to ``settings.litellm_*`` when the env vars are
    unset (so the orchestrator's existing configuration still works for
    DB-less callers running inside the pod).
    """
    api_key = os.environ.get(LITELLM_API_KEY_ENV_VAR) or settings.litellm_master_key
    if not api_key:
        raise MissingApiKeyError(
            LITELLM_API_KEY_ENV_VAR,
            f"LiteLLM proxy model '{model_name}' requires the "
            f"{LITELLM_API_KEY_ENV_VAR} environment variable to be set "
            f"for standalone (DB-less) operation.",
        )
    base_url = os.environ.get(LITELLM_API_BASE_ENV_VAR) or settings.litellm_api_base
    if not base_url:
        raise MissingApiKeyError(
            LITELLM_API_BASE_ENV_VAR,
            f"LiteLLM proxy model '{model_name}' requires the "
            f"{LITELLM_API_BASE_ENV_VAR} environment variable (or "
            f"settings.litellm_api_base) to be set for standalone "
            f"(DB-less) operation.",
        )
    logger.info("DB-less: using LiteLLM proxy for model: %s", model_name)
    return AsyncOpenAI(api_key=api_key, base_url=base_url, max_retries=1)


async def get_llm_client(
    user_id: UUID | None,
    model_name: str,
    db: AsyncSession | None,
) -> AsyncOpenAI:
    """
    Get configured LLM client for a user and model.

    Routing logic based on model prefix:
    - "builtin/model-name" → LiteLLM proxy (strips prefix)
    - "provider/model-name" → User's API key for that provider (BYOK)
    - No prefix → LiteLLM proxy (backward compat for old DB records)

    Supported BYOK providers are derived from BUILTIN_PROVIDERS (see top of file).

    When ``db`` is ``None`` (standalone / benchmark contexts), API keys are
    resolved from environment variables via :data:`BYOK_PROVIDER_ENV_VARS`
    and :data:`LITELLM_API_KEY_ENV_VAR` instead of the database. ``user_id``
    is ignored in that path.

    Args:
        user_id: The user ID. May be ``None`` when ``db`` is also ``None``.
        model_name: The model identifier (e.g., "builtin/gpt-4o", "gpt-4o", "openrouter/anthropic/claude-3.5-sonnet")
        db: Database session, or ``None`` for standalone env-var resolution.

    Returns:
        Configured AsyncOpenAI client ready to use

    Raises:
        ValueError: If user not found, provider not found, or API key not configured
    """
    if db is None:
        return _build_dbless_llm_client(model_name)

    from ..config import get_settings
    from ..models import User, UserProvider

    settings = get_settings()

    if user_id is None:
        raise ValueError("user_id is required when a database session is provided")

    # Get user
    result = await db.execute(select(User).where(User.id == user_id))
    user = result.scalar_one_or_none()
    if not user:
        raise ValueError(f"User {user_id} not found")

    # Handle builtin/ prefix — route to LiteLLM
    if model_name.startswith(BUILTIN_PREFIX):
        model_name = model_name[len(BUILTIN_PREFIX) :]
        # Fall through to the "no prefix" LiteLLM path below

    # Handle custom/ prefix — route directly to user's custom provider
    elif model_name.startswith(CUSTOM_PREFIX):
        stripped = model_name[len(CUSTOM_PREFIX) :]
        provider_slug = stripped.split("/")[0]

        result = await db.execute(
            select(UserProvider).where(
                UserProvider.user_id == user_id,
                UserProvider.slug == provider_slug,
                UserProvider.is_active.is_(True),
            )
        )
        custom_provider = result.scalar_one_or_none()

        if not custom_provider:
            raise ValueError(
                f"Custom provider '{provider_slug}' not found. Please add it in Library → API Keys."
            )

        provider_config = {
            "name": custom_provider.name,
            "base_url": custom_provider.base_url,
            "api_type": custom_provider.api_type,
            "default_headers": custom_provider.default_headers or {},
        }

        logger.info(f"Using custom provider {provider_config['name']} API for model: {model_name}")

        user_key_data = await get_user_api_key(user_id, provider_slug, db)
        effective_base_url = user_key_data["base_url"] or provider_config["base_url"]

        return AsyncOpenAI(
            api_key=user_key_data["key"],
            base_url=effective_base_url,
            default_headers=provider_config.get("default_headers", {}),
        )

    # Check if model has a built-in provider prefix (e.g., "openrouter/model-name")
    if "/" in model_name:
        provider_slug = model_name.split("/")[0]

        # Try built-in provider
        provider_config = get_builtin_provider_config(provider_slug)

        if not provider_config:
            # Unknown prefix — check if this model_id exists as a user's custom model
            # under a known provider (e.g. "z-ai/glm-5" stored under "openrouter")
            from ..models import UserCustomModel

            result = await db.execute(
                select(UserCustomModel).where(
                    UserCustomModel.user_id == user_id,
                    UserCustomModel.model_id == model_name,
                    UserCustomModel.is_active.is_(True),
                )
            )
            custom_model = result.scalar_one_or_none()

            if custom_model and custom_model.provider in BUILTIN_PROVIDERS:
                # Re-route through the correct provider
                provider_slug = custom_model.provider
                provider_config = BUILTIN_PROVIDERS[provider_slug]
                # Rewrite model_name with provider prefix so stripping works correctly
                model_name = f"{provider_slug}/{model_name}"
                logger.info(f"Resolved unprefixed model to {provider_slug}: {model_name}")
            else:
                raise ValueError(
                    f"Unknown provider '{provider_slug}'. "
                    f"Available providers: {', '.join(BUILTIN_PROVIDERS.keys())}. "
                    f"Custom providers must use the 'custom/' prefix."
                )

        logger.info(f"Using {provider_config['name']} API for model: {model_name}")

        # Get user's API key and optional base URL override for this provider
        user_key_data = await get_user_api_key(user_id, provider_slug, db)
        effective_base_url = user_key_data["base_url"] or provider_config["base_url"]

        # Return client configured for the provider
        return AsyncOpenAI(
            api_key=user_key_data["key"],
            base_url=effective_base_url,
            default_headers=provider_config.get("default_headers", {}),
        )
    else:
        # No prefix — use LiteLLM proxy for system models.
        # In desktop mode (no LiteLLM proxy) fall back to common env-var API
        # keys so users can drop OPENAI_API_KEY / ANTHROPIC_API_KEY into
        # $OPENSAIL_HOME/.env without needing to configure BYOK in the UI.
        import os as _os

        if not user.litellm_api_key or not settings.litellm_api_base:
            # Desktop / standalone: no per-user LiteLLM key provisioned.

            # Desktop + paired to a Tesslate Cloud account: route system
            # models through the cloud companion's OpenAI-compatible proxy
            # (POST {cloud}/api/v1/chat/completions) so the user spends their
            # account credits. The proxy runs credit checks and forwards to
            # the internal LiteLLM — LiteLLM itself is never exposed to the
            # desktop client. Pairing is a deliberate action, so it wins over
            # the env-var / BYOK fallbacks below.
            if settings.is_desktop_mode:
                from . import token_store  # noqa: PLC0415
                from .cloud_config import get_cloud_url  # noqa: PLC0415

                cloud_token = token_store.get_cloud_token()
                if cloud_token:
                    logger.info(
                        "Desktop paired; routing model %s through cloud proxy",
                        model_name,
                    )
                    return AsyncOpenAI(
                        api_key=cloud_token,
                        base_url=f"{get_cloud_url()}/api/v1",
                        max_retries=1,
                    )

            # If a LiteLLM proxy is configured (LITELLM_API_BASE +
            # LITELLM_MASTER_KEY both set — e.g. from $OPENSAIL_HOME/.env),
            # use the master key directly so the proxy is reachable without
            # the full cloud user-key provisioning flow.
            if settings.litellm_api_base and settings.litellm_master_key:
                logger.info(
                    "User has no litellm_api_key; using master key for LiteLLM proxy (model=%s)",
                    model_name,
                )
                return AsyncOpenAI(
                    api_key=settings.litellm_master_key,
                    base_url=settings.litellm_api_base,
                    max_retries=1,
                )

            _env_fallbacks = [
                # (env_var, base_url, model_rewrite_prefix)
                ("OPENAI_API_KEY", "https://api.openai.com/v1", None),
                ("ANTHROPIC_API_KEY", "https://api.anthropic.com/v1", None),
                ("OPENROUTER_API_KEY", "https://openrouter.ai/api/v1", None),
            ]
            for env_var, base_url, _ in _env_fallbacks:
                api_key = _os.environ.get(env_var)
                if api_key:
                    logger.info(
                        "LiteLLM proxy not configured; falling back to %s env var for model %s",
                        env_var,
                        model_name,
                    )
                    return AsyncOpenAI(api_key=api_key, base_url=base_url, max_retries=1)

            # Last resort: if user has any active BYOK provider, use it.
            # This lets unprefixed default models (e.g. "claude-sonnet-4.6")
            # work when the LiteLLM proxy isn't configured.
            if db is not None and user_id is not None:
                from ..models import UserAPIKey  # noqa: PLC0415
                from ..routers.secrets import decode_key  # noqa: PLC0415

                byok_result = await db.execute(
                    select(UserAPIKey)
                    .where(
                        UserAPIKey.user_id == user_id,
                        UserAPIKey.is_active.is_(True),
                    )
                    .limit(1)
                )
                any_key = byok_result.scalar_one_or_none()
                if any_key and any_key.provider in BUILTIN_PROVIDERS:
                    provider_cfg = BUILTIN_PROVIDERS[any_key.provider]
                    logger.info(
                        "No LiteLLM proxy; auto-routing unprefixed model '%s' to user's %s BYOK key",
                        model_name,
                        any_key.provider,
                    )
                    return AsyncOpenAI(
                        api_key=decode_key(any_key.encrypted_value),
                        base_url=any_key.base_url or provider_cfg["base_url"],
                        default_headers=provider_cfg.get("default_headers", {}),
                        max_retries=1,
                    )

            raise ValueError(
                "No LLM access configured. "
                "Sign in to Tesslate Cloud in Settings → Cloud to use your "
                "account credits, add a provider key in Library → API Keys, or "
                "set OPENAI_API_KEY / ANTHROPIC_API_KEY / OPENROUTER_API_KEY in "
                "$OPENSAIL_HOME/.env."
            )

        logger.info(f"Using LiteLLM proxy for model: {model_name}")
        return AsyncOpenAI(
            api_key=user.litellm_api_key, base_url=settings.litellm_api_base, max_retries=1
        )


class ModelAdapter(ABC):
    """
    Abstract base class for model adapters.

    All adapters must implement the chat() method which streams the model's text response.
    """

    @abstractmethod
    async def chat(self, messages: list[dict[str, str]], **kwargs) -> AsyncGenerator[str, None]:
        """
        Send messages to the model and stream text response chunks.

        Args:
            messages: List of message dicts with "role" and "content"
            **kwargs: Model-specific parameters (temperature, max_tokens, etc.)

        Yields:
            Text chunks as they're generated by the model
        """
        pass

    async def chat_with_tools(
        self,
        messages: list[dict],
        tools: list[dict] | None = None,
        tool_choice: str = "auto",
        stream: bool = False,
        **kwargs: Any,
    ) -> dict | AsyncGenerator[dict, None]:
        """
        Call the model with optional tool definitions.

        When ``stream=False`` (default) returns a dict with keys:
          ``content``, ``tool_calls``, ``usage``, ``finish_reason``, ``raw_response``.
        When ``stream=True`` returns an async iterator of delta dicts.
        """
        raise NotImplementedError(
            f"{self.__class__.__name__} does not support chat_with_tools(). "
            f"Use an adapter that supports native function calling (e.g., OpenAIAdapter)."
        )
        yield {}  # Make it a generator

    @abstractmethod
    def get_model_name(self) -> str:
        """Get the model name/identifier."""
        pass


THINKING_BUDGET = {"xhigh": 32_000, "high": 16_000, "medium": 8_000, "low": 4_000}

# Adaptive effort values for Claude 4.6 models (uses output_config.effort
# instead of manual budget_tokens)
_ADAPTIVE_EFFORT_MAP = {
    "xhigh": "max",
    "high": "high",
    "medium": "medium",
    "low": "low",
}

# Models that support extended thinking via the OpenAI-compat API
_THINKING_CAPABLE_PATTERNS = (
    "claude-opus-4",
    "claude-sonnet-4",
    "claude-3-7",
    "claude-3.7",
    "deepseek-r1",
    "deepseek-reasoner",
)


def _is_thinking_capable(model_name: str) -> bool:
    """Check if a model supports extended thinking. Haiku does NOT."""
    lower = model_name.lower()
    if "haiku" in lower:
        return False
    return any(p in lower for p in _THINKING_CAPABLE_PATTERNS)


def _supports_adaptive_thinking(model_name: str) -> bool:
    """Return True for Claude 4.6 models that support adaptive thinking."""
    return any(v in model_name for v in ("4-6", "4.6"))


class OpenAIAdapter(ModelAdapter):
    """
    Adapter for OpenAI models (GPT-4, GPT-3.5-turbo, etc.)
    Also works with OpenAI-compatible APIs like Cerebras, Groq, Together AI, etc.
    """

    def __init__(
        self,
        model_name: str,
        client: AsyncOpenAI,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        thinking_effort: str = "",
    ):
        """
        Initialize OpenAI adapter with a pre-configured client.

        Args:
            model_name: Model identifier (e.g., "gpt-4o", "openrouter/anthropic/claude-3.5-sonnet")
            client: Pre-configured AsyncOpenAI client (from get_llm_client())
            temperature: Sampling temperature (0-2)
            max_tokens: Maximum tokens in response
            thinking_effort: "", "low", "medium", "high", "xhigh" — enables extended thinking
        """
        self.model_name = model_name
        self.client = client
        self.temperature = temperature
        self.max_tokens = max_tokens
        self.thinking_effort = thinking_effort

        logger.info(f"OpenAIAdapter initialized - model: {model_name}")

    async def chat(self, messages: list[dict[str, str]], **kwargs) -> AsyncGenerator[str, None]:
        """
        Send messages to OpenAI API and stream response chunks.

        Args:
            messages: List of message dicts
            **kwargs: Override temperature, max_tokens, etc.

        Yields:
            Text chunks as they're generated by the model
        """
        _ = kwargs.get("temperature", self.temperature)  # Reserved for future use
        max_tokens = kwargs.get("max_tokens", self.max_tokens)

        request_params = {
            "model": self.model_name,
            "messages": messages,
            "stream": True,  # Enable streaming
            "stream_options": {"include_usage": True},
        }

        if max_tokens:
            request_params["max_tokens"] = max_tokens

        try:
            # Inject prompt caching breakpoints for eligible models (e.g. Claude).
            from .prompt_caching import inject_cache_breakpoints

            inject_cache_breakpoints(messages, self.model_name)

            logger.debug(
                f"Sending streaming request to {self.model_name} with {len(messages)} messages"
            )

            # Create streaming completion
            stream = await self.client.chat.completions.create(**request_params)

            # Stream chunks as they arrive
            self._last_usage = None
            async for chunk in stream:
                if chunk.choices and len(chunk.choices) > 0:
                    delta = chunk.choices[0].delta
                    if delta.content:
                        yield delta.content
                # Capture usage from the final chunk (no choices, has usage)
                if hasattr(chunk, "usage") and chunk.usage:
                    self._last_usage = {
                        "prompt_tokens": getattr(chunk.usage, "prompt_tokens", 0),
                        "completion_tokens": getattr(chunk.usage, "completion_tokens", 0),
                    }
                    # Capture Anthropic/Bedrock cache metrics when present
                    for field in (
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                        "cached_tokens",
                    ):
                        val = getattr(chunk.usage, field, None)
                        if val:
                            self._last_usage[field] = val

            logger.debug(f"Streaming complete for {self.model_name}")

        except Exception as e:
            logger.error(f"OpenAI API streaming error: {e}", exc_info=True)
            raise RuntimeError(f"Model API error: {str(e)}") from e

    async def chat_with_tools(
        self,
        messages: list[dict],
        tools: list[dict] | None = None,
        tool_choice: str = "auto",
        stream: bool = False,
        **kwargs: Any,
    ) -> dict | AsyncGenerator[dict, None]:
        """
        Call the model with optional tool definitions.

        When ``stream=False`` (default used by TesslateAgent) makes a single
        non-streaming request and returns a dict with:
          ``content``, ``tool_calls``, ``usage``, ``finish_reason``.

        When ``stream=True`` returns an async generator of delta dicts.
        """
        request_params: dict[str, Any] = {
            "model": self.model_name,
            "messages": messages,
            "tool_choice": tool_choice,
        }
        if tools:
            request_params["tools"] = tools
            request_params["parallel_tool_calls"] = True

        if self.max_tokens:
            request_params["max_tokens"] = self.max_tokens

        # Extended thinking for capable models.
        if self.thinking_effort and _is_thinking_capable(self.model_name):
            budget = THINKING_BUDGET.get(self.thinking_effort, 0)
            if budget:
                extra_body: dict[str, Any] = dict(request_params.get("extra_body") or {})
                if _supports_adaptive_thinking(self.model_name):
                    extra_body["thinking"] = {"type": "adaptive"}
                    extra_body["output_config"] = {
                        "effort": _ADAPTIVE_EFFORT_MAP.get(self.thinking_effort, "medium")
                    }
                else:
                    extra_body["thinking"] = {"type": "enabled", "budget_tokens": budget}
                request_params["extra_body"] = extra_body

        # Inject prompt caching breakpoints for eligible models (e.g. Claude).
        try:
            from .prompt_caching import inject_cache_breakpoints

            inject_cache_breakpoints(messages, self.model_name)
        except Exception:
            pass

        n_tools = len(tools) if tools else 0
        logger.debug(
            "chat_with_tools: %s, %d messages, %d tools, stream=%s",
            self.model_name,
            len(messages),
            n_tools,
            stream,
        )

        if stream:
            return self._stream_with_tools(request_params)

        # Non-streaming path — accumulate and return the submodule-contract dict.
        try:
            response = await self.client.chat.completions.create(**request_params)
        except Exception as e:
            logger.error("chat_with_tools error: %s", e, exc_info=True)
            raise RuntimeError(f"Model API error: {e}") from e

        return self._format_response(response)

    def _format_response(self, response: Any) -> dict[str, Any]:
        """Convert a non-streaming OpenAI response into the adapter-contract dict."""
        choice = response.choices[0]
        message = choice.message
        tool_calls: list[dict[str, Any]] = []
        for tc in getattr(message, "tool_calls", None) or []:
            func = getattr(tc, "function", None)
            tool_calls.append(
                {
                    "id": getattr(tc, "id", ""),
                    "type": "function",
                    "function": {
                        "name": getattr(func, "name", "") if func else "",
                        "arguments": getattr(func, "arguments", "") if func else "",
                    },
                }
            )
        usage: dict[str, Any] = {}
        raw_usage = getattr(response, "usage", None)
        if raw_usage is not None:
            usage["prompt_tokens"] = getattr(raw_usage, "prompt_tokens", 0) or 0
            usage["completion_tokens"] = getattr(raw_usage, "completion_tokens", 0) or 0
            usage["total_tokens"] = getattr(raw_usage, "total_tokens", 0) or 0
            for field in (
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "cached_tokens",
            ):
                val = getattr(raw_usage, field, None)
                if val:
                    usage[field] = val
        return {
            "content": getattr(message, "content", "") or "",
            "tool_calls": tool_calls,
            "usage": usage,
            "finish_reason": getattr(choice, "finish_reason", None),
            "raw_response": response,
        }

    async def _stream_with_tools(
        self, request_params: dict[str, Any]
    ) -> AsyncGenerator[dict, None]:
        """Async generator for the streaming path of chat_with_tools."""
        try:
            params = {**request_params, "stream": True, "stream_options": {"include_usage": True}}
            api_stream = await self.client.chat.completions.create(**params)

            tool_calls_data: dict[int, dict[str, Any]] = {}
            content_text = ""
            finish_reason = None
            usage_data: dict[str, Any] | None = None

            async for chunk in api_stream:
                if hasattr(chunk, "usage") and chunk.usage:
                    usage_data = {
                        "prompt_tokens": getattr(chunk.usage, "prompt_tokens", 0),
                        "completion_tokens": getattr(chunk.usage, "completion_tokens", 0),
                    }
                    for field in (
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                        "cached_tokens",
                    ):
                        val = getattr(chunk.usage, field, None)
                        if val:
                            usage_data[field] = val
                if not chunk.choices:
                    continue
                choice = chunk.choices[0]
                delta = choice.delta
                if choice.finish_reason:
                    finish_reason = choice.finish_reason
                if delta.content:
                    content_text += delta.content
                    yield {"type": "text_delta", "content": delta.content}
                if delta.tool_calls:
                    for tc_delta in delta.tool_calls:
                        idx = tc_delta.index
                        if idx not in tool_calls_data:
                            tool_calls_data[idx] = {
                                "id": "",
                                "function": {"name": "", "arguments": ""},
                            }
                        if tc_delta.id:
                            tool_calls_data[idx]["id"] = tc_delta.id
                        if tc_delta.function:
                            if tc_delta.function.name:
                                tool_calls_data[idx]["function"]["name"] = tc_delta.function.name
                            if tc_delta.function.arguments:
                                tool_calls_data[idx]["function"]["arguments"] += (
                                    tc_delta.function.arguments
                                )

            if tool_calls_data:
                sorted_calls = [tool_calls_data[i] for i in sorted(tool_calls_data)]
                yield {"type": "tool_calls_complete", "tool_calls": sorted_calls}

            yield {
                "type": "done",
                "finish_reason": finish_reason or ("tool_calls" if tool_calls_data else "stop"),
                "usage": usage_data,
            }

            logger.debug(
                "chat_with_tools stream complete: %d chars, %d tool calls",
                len(content_text),
                len(tool_calls_data),
            )
        except Exception as e:
            logger.error("chat_with_tools stream error: %s", e, exc_info=True)
            raise RuntimeError(f"Model API error: {e}") from e

    def get_model_name(self) -> str:
        return self.model_name


async def create_model_adapter(
    model_name: str,
    user_id: UUID | None = None,
    db: AsyncSession | None = None,
    provider: str | None = None,
    **kwargs,
) -> ModelAdapter:
    """
    Factory function to create the appropriate model adapter.

    Uses get_llm_client() to handle model routing (OpenRouter vs LiteLLM).
    Auto-detects provider from model name if not specified.

    Two modes of operation:

    1. **DB-backed** (``db`` is a live ``AsyncSession``): API keys are
       resolved from the user's ``UserAPIKey`` records. ``user_id`` is
       required. Behavior is unchanged from previous releases — this
       branch is the default path for the orchestrator request/chat flow.
    2. **Standalone / DB-less** (``db is None``): API keys are resolved
       from environment variables. See :data:`BYOK_PROVIDER_ENV_VARS`
       for the provider → env-var mapping and :data:`LITELLM_API_KEY_ENV_VAR`
       / :data:`LITELLM_API_BASE_ENV_VAR` for the LiteLLM proxy path.
       ``user_id`` is optional and ignored in this path. This branch is
       selected by passing ``db=None`` (no separate flag) so existing
       callers are unaffected. It is intended for standalone CLI tools,
       benchmark harnesses, and other contexts that do not have
       database connectivity.

    In the standalone path, bare model names are routed by prefix:

    - ``gpt-*``, ``o1-*``, ``o3-*``, ``o4-*`` → OpenAI direct
      (``OPENAI_API_KEY``).
    - ``claude-*`` → Anthropic direct if ``ANTHROPIC_API_KEY`` is set,
      otherwise OpenRouter if ``OPENROUTER_API_KEY`` is set, otherwise
      the LiteLLM proxy.
    - Anything else falls through to the LiteLLM proxy.

    Args:
        model_name: Model identifier (e.g., "gpt-4o",
            "openrouter/anthropic/claude-3.5-sonnet", "builtin/claude-opus-4.6").
        user_id: User ID for fetching API keys (required when ``db`` is
            set; ignored when ``db`` is ``None``).
        db: Database session, or ``None`` for env-var resolution.
        provider: Force specific provider ("openai", "anthropic", etc.).
            When ``db`` is ``None`` the provider defaults to ``"openai"``
            regardless of the model prefix, because every supported
            standalone provider exposes an OpenAI-compatible endpoint.
        **kwargs: Additional adapter parameters (temperature, max_tokens,
            thinking_effort, etc.). These flow through to the underlying
            :class:`OpenAIAdapter` unchanged.

    Returns:
        ModelAdapter instance.

    Raises:
        MissingApiKeyError: In the standalone path, when the required
            provider env var is unset. The ``env_var`` attribute names
            the variable to export.
        ValueError: For non-recoverable routing errors (unknown provider
            prefix, ``custom/`` prefix without a DB session, etc.).

    Examples:
        # OpenAI GPT-4 (via LiteLLM, DB-backed)
        adapter = await create_model_adapter("gpt-4o", user_id=uid, db=db)

        # OpenRouter model (uses user's OpenRouter key)
        adapter = await create_model_adapter("openrouter/anthropic/claude-3.5-sonnet", user_id=uid, db=db)

        # Standalone/benchmark — reads OPENAI_API_KEY from the environment
        adapter = await create_model_adapter("openai/gpt-4o-mini", db=None)

        # Standalone bare name — routes to OpenAI direct via OPENAI_API_KEY
        adapter = await create_model_adapter("gpt-4o", db=None)
    """
    # Auto-detect API type from model prefix using the provider registry.
    #
    # Standalone / DB-less path (db is None): we always use the OpenAI
    # chat.completions client because every public provider we route to
    # exposes an OpenAI-compatible endpoint (including Anthropic's
    # ``/v1/`` beta surface). This lets benchmark harnesses pass
    # ``anthropic/claude-3-5-sonnet`` without also having to pass
    # ``provider="openai"`` to dodge the ``api_type: "anthropic"`` entry
    # in BUILTIN_PROVIDERS.
    if not provider:
        if db is None:
            provider = "openai"
        elif "/" in model_name:
            slug = model_name.split("/", 1)[0]
            cfg = BUILTIN_PROVIDERS.get(slug)
            provider = cfg["api_type"] if cfg else "openai"
        else:
            provider = "openai"

    # Both "openai" and "anthropic" providers are served via OpenAI-compatible
    # endpoints. Anthropic exposes an OpenAI-compatible /v1 surface (Bearer
    # token auth, chat.completions format) that works with AsyncOpenAI.
    # get_llm_client() already configures the client with the right base URL
    # and API key for each provider, so we use the same adapter for both.
    if provider in ("openai", "anthropic"):
        # Get configured client using centralized routing
        client = await get_llm_client(user_id, model_name, db)

        # Strip routing prefix from model name before passing to adapter
        # builtin/gpt-4o → gpt-4o (LiteLLM models)
        # custom/my-ollama/neural-7b → neural-7b (custom provider)
        # openai/gpt-5.2 → gpt-5.2, openrouter/anthropic/claude → anthropic/claude (BYOK)
        # anthropic/claude-3.5-sonnet → claude-3.5-sonnet
        api_model_name = model_name
        if model_name.startswith(BUILTIN_PREFIX):
            api_model_name = model_name[len(BUILTIN_PREFIX) :]
        elif model_name.startswith(CUSTOM_PREFIX):
            # Strip "custom/{slug}/" to get bare model name for the API call
            stripped = model_name[len(CUSTOM_PREFIX) :]
            parts = stripped.split("/", 1)
            api_model_name = parts[1] if len(parts) > 1 else parts[0]
        elif "/" in model_name:
            # Only strip the first segment if it's a known provider prefix
            # e.g. "openrouter/z-ai/glm-5" → "z-ai/glm-5" (strip "openrouter/")
            # but  "z-ai/glm-5" → "z-ai/glm-5" (keep as-is, it's the full model name)
            first_seg = model_name.split("/", 1)[0]
            if first_seg in BUILTIN_PROVIDERS:
                api_model_name = model_name.split("/", 1)[1]

        # Create adapter with the configured client
        return OpenAIAdapter(model_name=api_model_name, client=client, **kwargs)
    else:
        raise ValueError(f"Unsupported provider: {provider}")
