import asyncio
import json
import os
import uuid

import pytest

httpx = pytest.importorskip("httpx")

pytestmark = pytest.mark.integration


def _base_url() -> str:
    port = os.environ.get("REFACT_LSP_PORT")
    if not port:
        pytest.skip("REFACT_LSP_PORT is unset; live refact-lsp integration tests skipped")
    return f"http://127.0.0.1:{port}"


async def _post_command(client: httpx.AsyncClient, base_url: str, chat_id: str, payload: dict) -> None:
    response = await client.post(
        f"{base_url}/v1/chats/{chat_id}/commands",
        json={"client_request_id": str(uuid.uuid4()), **payload},
    )
    assert response.status_code == 202, response.text


async def _read_sse_event(lines) -> dict:
    async for line in lines:
        if line.startswith("data: "):
            return json.loads(line[6:])
    raise AssertionError("SSE stream ended before an event arrived")


async def _collect_until(
    base_url: str,
    chat_id: str,
    predicate,
    timeout: float = 30.0,
    existing_events: list[dict] | None = None,
) -> list[dict]:
    events: list[dict] = list(existing_events or [])
    if predicate(events):
        return events
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


def _events_have_stream_cycle(events: list[dict]) -> bool:
    seen = {event.get("type") for event in events}
    return {"stream_started", "stream_finished"}.issubset(seen)


def _tool_result_text(events: list[dict]) -> str:
    chunks: list[str] = []
    for event in events:
        if event.get("type") == "stream_delta":
            for op in event.get("ops", []):
                chunks.append(json.dumps(op))
        if event.get("type") in {"message_added", "message_updated"}:
            chunks.append(json.dumps(event.get("message", {})))
    return "\n".join(chunks)


def _background_updates(events: list[dict]) -> list[dict]:
    return [event for event in events if event.get("type") == "background_agent_updated"]


def _has_background_finished_queue_message(events: list[dict], kind: str) -> bool:
    needle = f"[background {kind} finished]"
    for event in events:
        if event.get("type") == "queue_updated":
            if needle in json.dumps(event):
                return True
        if event.get("type") in {"message_added", "message_updated"}:
            if needle in json.dumps(event.get("message", {})):
                return True
    return False


@pytest.mark.asyncio
async def test_subagent_background_default():
    base_url = _base_url()
    chat_id = f"test-bg-subagent-{uuid.uuid4().hex[:8]}"

    async with httpx.AsyncClient(timeout=10.0) as client:
        await _post_command(
            client,
            base_url,
            chat_id,
            {"type": "set_params", "patch": {"mode": "agent"}},
        )
        await _post_command(
            client,
            base_url,
            chat_id,
            {
                "type": "user_message",
                "content": "Call subagent(task='inspect README', expected_result='summary') once and continue.",
            },
        )

    events = await _collect_until(
        base_url,
        chat_id,
        lambda seen: _events_have_stream_cycle(seen)
        and "background_agent_id" in _tool_result_text(seen)
        and bool(_background_updates(seen)),
    )
    assert "background_agent_id" in _tool_result_text(events)
    assert _background_updates(events)

    followup = await _collect_until(
        base_url,
        chat_id,
        lambda seen: _has_background_finished_queue_message(seen, "subagent"),
        timeout=60.0,
        existing_events=events,
    )
    assert _has_background_finished_queue_message(followup, "subagent")


@pytest.mark.asyncio
async def test_delegate_background_with_target_files():
    base_url = _base_url()
    chat_id = f"test-bg-delegate-{uuid.uuid4().hex[:8]}"
    target_files = ["tests/emergency_frog_situation/frog.py"]

    async with httpx.AsyncClient(timeout=10.0) as client:
        await _post_command(client, base_url, chat_id, {"type": "set_params", "patch": {"mode": "agent"}})
        await _post_command(
            client,
            base_url,
            chat_id,
            {
                "type": "user_message",
                "content": (
                    "Call delegate(description='touch frog', prompt='inspect only', "
                    "expected_result='report', target_files=['tests/emergency_frog_situation/frog.py']) once."
                ),
            },
        )

    events = await _collect_until(
        base_url,
        chat_id,
        lambda seen: any(
            update.get("agent", {}).get("targetFiles") == target_files
            or update.get("agent", {}).get("target_files") == target_files
            for update in _background_updates(seen)
        ),
    )
    assert any(
        update.get("agent", {}).get("targetFiles") == target_files
        or update.get("agent", {}).get("target_files") == target_files
        for update in _background_updates(events)
    )


@pytest.mark.asyncio
async def test_concurrent_delegates_overlap_warning():
    base_url = _base_url()
    chat_id = f"test-bg-overlap-{uuid.uuid4().hex[:8]}"

    async with httpx.AsyncClient(timeout=10.0) as client:
        await _post_command(client, base_url, chat_id, {"type": "set_params", "patch": {"mode": "agent"}})
        await _post_command(
            client,
            base_url,
            chat_id,
            {
                "type": "user_message",
                "content": (
                    "Start two delegate calls in parallel with the same "
                    "target_files=['tests/emergency_frog_situation/frog.py'] and report the warning."
                ),
            },
        )

    events = await _collect_until(
        base_url,
        chat_id,
        lambda seen: "overlap" in _tool_result_text(seen).lower()
        or any("overlap" in json.dumps(update).lower() for update in _background_updates(seen)),
    )
    assert "overlap" in (json.dumps(events).lower())


@pytest.mark.asyncio
async def test_agent_wait_and_cancel():
    base_url = _base_url()
    chat_id = f"test-bg-cancel-{uuid.uuid4().hex[:8]}"

    async with httpx.AsyncClient(timeout=10.0) as client:
        await _post_command(client, base_url, chat_id, {"type": "set_params", "patch": {"mode": "agent"}})
        await _post_command(
            client,
            base_url,
            chat_id,
            {
                "type": "user_message",
                "content": (
                    "Start a delegate with target_files=['tests/emergency_frog_situation/frog.py'], "
                    "then call agent_wait(agent_id, timeout_ms=10), then agent_cancel(agent_id)."
                ),
            },
        )

    events = await _collect_until(
        base_url,
        chat_id,
        lambda seen: "agent_wait" in _tool_result_text(seen)
        and ("cancelled" in _tool_result_text(seen).lower() or "canceled" in _tool_result_text(seen).lower()),
        timeout=60.0,
    )
    text = _tool_result_text(events).lower()
    assert "agent_wait" in text
    assert "cancelled" in text or "canceled" in text
