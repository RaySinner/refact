import asyncio
import json
import os
import uuid
from pathlib import Path

import pytest

httpx = pytest.importorskip("httpx")

def _base_url() -> str:
    port = os.environ.get("REFACT_LSP_PORT")
    if not port:
        pytest.skip("REFACT_LSP_PORT is unset; live refact-lsp integration tests skipped")
    return f"http://127.0.0.1:{port}"


def _workspace_root() -> Path:
    return Path(__file__).resolve().parents[2]


async def _post_command(client: httpx.AsyncClient, base_url: str, chat_id: str, payload: dict) -> dict:
    response = await client.post(
        f"{base_url}/v1/chats/{chat_id}/commands",
        json={"client_request_id": str(uuid.uuid4()), **payload},
    )
    assert response.status_code in (200, 202), response.text
    return response.json()


async def _init_workspace(client: httpx.AsyncClient, base_url: str) -> None:
    response = await client.post(
        f"{base_url}/v1/lsp-initialize",
        json={"project_roots": [_workspace_root().as_uri()]},
    )
    assert response.status_code == 200, response.text


async def _read_sse_event(lines) -> dict:
    async for line in lines:
        if line.startswith("data: "):
            return json.loads(line[6:])
    raise AssertionError("SSE stream ended before an event arrived")


async def _collect_until(base_url: str, chat_id: str, predicate, timeout: float = 30.0) -> list[dict]:
    events: list[dict] = []
    async with httpx.AsyncClient(timeout=None) as client:
        async with client.stream(
            "GET",
            f"{base_url}/v1/chats/subscribe",
            params={"chat_id": chat_id},
        ) as response:
            assert response.status_code == 200, response.text
            lines = response.aiter_lines()
            deadline = asyncio.get_running_loop().time() + timeout
            while True:
                remaining = deadline - asyncio.get_running_loop().time()
                if remaining <= 0:
                    raise AssertionError(f"timed out waiting for events; saw {events!r}")
                event = await asyncio.wait_for(_read_sse_event(lines), timeout=remaining)
                events.append(event)
                if predicate(events):
                    return events


def _message(event: dict) -> dict:
    return event.get("message") or {}


def _event_meta(message: dict) -> dict:
    return message.get("extra", {}).get("event") or {}


def _plan_meta(message: dict) -> dict:
    return message.get("extra", {}).get("plan") or {}


def _messages(events: list[dict]) -> list[dict]:
    result: list[dict] = []
    for event in events:
        if event.get("type") == "snapshot":
            result.extend(event.get("messages") or [])
        if event.get("type") == "message_added":
            result.append(_message(event))
    return result


def _event_message(events: list[dict], subkind: str) -> dict | None:
    for message in _messages(events):
        if message.get("role") == "event" and _event_meta(message).get("subkind") == subkind:
            return message
    return None


def _plan_message(events: list[dict], version: int) -> dict | None:
    for message in _messages(events):
        if message.get("role") == "plan" and _plan_meta(message).get("version") == version:
            return message
    return None


def _tool_call(name: str, arguments: dict, call_id: str | None = None) -> dict:
    return {
        "id": call_id or f"call-{uuid.uuid4().hex[:8]}",
        "type": "function",
        "function": {
            "name": name,
            "arguments": json.dumps(arguments),
        },
    }


async def _execute_tool(
    client: httpx.AsyncClient,
    base_url: str,
    chat_id: str,
    name: str,
    arguments: dict,
    *,
    model_name: str = "gpt-4o-mini",
) -> list[dict]:
    assistant = {
        "role": "assistant",
        "content": "",
        "tool_calls": [_tool_call(name, arguments)],
    }
    response = await client.post(
        f"{base_url}/v1/tools-execute",
        json={
            "messages": [{"role": "user", "content": f"Call {name}"}, assistant],
            "n_ctx": 4096,
            "maxgen": 256,
            "subchat_tool_parameters": {},
            "postprocess_parameters": {
                "use_ast_based_pp": True,
                "useful_background": 5.0,
                "useful_symbol_default": 10.0,
                "downgrade_parent_coef": 0.6,
                "downgrade_body_coef": 0.8,
                "comments_propagate_up_coef": 0.99,
                "close_small_gaps": True,
                "take_floor": 0.0,
                "max_files_n": 0,
            },
            "model_name": model_name,
            "chat_id": chat_id,
            "style": None,
        },
    )
    assert response.status_code == 200, response.text
    data = response.json()
    assert data["tools_ran"] is True
    return data["messages"]


def _tool_json(message: dict) -> dict:
    content = message.get("content")
    if isinstance(content, str):
        return json.loads(content)
    return content


@pytest.mark.asyncio
async def test_mode_switch_installs_plan_and_emits_event():
    base_url = _base_url()
    chat_id = f"test-hidden-mode-{uuid.uuid4().hex[:8]}"

    async def drive() -> None:
        async with httpx.AsyncClient(timeout=10.0) as client:
            await _post_command(
                client,
                base_url,
                chat_id,
                {
                    "type": "set_params",
                    "patch": {"mode": "task_agent", "reason": "Install an integration-test plan"},
                },
            )

    waiter = asyncio.create_task(
        _collect_until(
            base_url,
            chat_id,
            lambda seen: _event_message(seen, "mode_switch") is not None
            and _plan_message(seen, 1) is not None,
        )
    )
    await asyncio.sleep(0.1)
    await drive()
    events = await waiter

    mode_event = _event_message(events, "mode_switch")
    assert mode_event is not None
    assert _event_meta(mode_event)["source"] == "chat.session"
    plan = _plan_message(events, 1)
    assert plan is not None
    assert _plan_meta(plan)["mode"] == "task_agent"


@pytest.mark.asyncio
async def test_set_plan_tool_creates_v2():
    base_url = _base_url()
    chat_id = f"test-hidden-set-plan-{uuid.uuid4().hex[:8]}"

    async with httpx.AsyncClient(timeout=10.0) as client:
        waiter_v1 = asyncio.create_task(
            _collect_until(base_url, chat_id, lambda seen: _plan_message(seen, 1) is not None)
        )
        await asyncio.sleep(0.1)
        await _post_command(client, base_url, chat_id, {"type": "set_params", "patch": {"mode": "task_agent"}})
        await waiter_v1

        messages = await _execute_tool(
            client,
            base_url,
            chat_id,
            "set_plan",
            {"content": "## Updated plan\n- Verify hidden role v2", "summary": "E2E v2"},
        )

    result = _tool_json(messages[-1])
    assert result["version"] == 2

    events = await _collect_until(base_url, chat_id, lambda seen: _plan_message(seen, 2) is not None)
    plan = _plan_message(events, 2)
    assert plan is not None
    assert _plan_meta(plan)["version"] == 2
    assert "Verify hidden role v2" in plan.get("content", "")
