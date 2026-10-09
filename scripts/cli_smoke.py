"""Check CLI commands against the existing offline chat lifecycle fixture."""

import json
import os
import signal
import subprocess
import time


def check_cli_commands(cli_binary, endpoint, token_path, env, root,
                       session_receipt, message_receipt, reply):
    prefix = [str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path)]
    session_id = session_receipt["session_id"]
    turn_id = message_receipt["turn_id"]

    def invoke(arguments, json_output=True, input_text=None):
        command = prefix + (["--json"] if json_output else []) + arguments
        result = subprocess.run(command, env=env, input=input_text, check=True,
                                capture_output=True, text=True, timeout=15)
        if not json_output:
            return result.stdout
        return [json.loads(line) for line in result.stdout.splitlines() if line.strip()]

    session, = invoke(["session", "show", session_id])
    models, = invoke(["models"])
    model_id = f'{session["provider"]}/{session["model"]}'
    if not any(model["id"] == model_id and model["ready"] for model in models):
        raise RuntimeError(f"CLI model discovery missed the ready session model: {models}")
    sessions, = invoke(["session", "list", "--after", "0", "--limit", "1"])
    if [item["id"] for item in sessions["items"]] != [session_id]:
        raise RuntimeError(f"CLI session listing missed the accepted conversation: {sessions}")
    first_page, = invoke(["session", "history", session_id, "--limit", "1"])
    if len(first_page["items"]) != 1 or first_page["items"][0]["role"] != "user":
        raise RuntimeError(f"CLI history did not preserve pagination/order: {first_page}")
    if first_page["next_after"] is None:
        raise RuntimeError("CLI history omitted its continuation cursor")
    second_page, = invoke([
        "session", "history", session_id, "--after", str(first_page["next_after"]),
    ])
    if len(second_page["items"]) != 1 or second_page["items"][0]["text"] != reply:
        raise RuntimeError(f"CLI history cursor missed the canonical reply: {second_page}")
    turn, = invoke(["turn", "show", turn_id])
    if session["id"] != session_id or turn["status"] != "completed":
        raise RuntimeError(f"CLI inspection did not report the completed turn: {session} {turn}")

    # Reuse an accepted command, so all prompt sources and turn following can
    # be checked without dispatching additional provider requests.
    text = first_page["items"][0]["text"]
    prompt_file = root / "cli-prompt.txt"
    prompt_file.write_text(text, encoding="utf-8")
    retry_options = ["--command-id", message_receipt["command_id"]]
    retried = invoke([
        "chat", "--model", model_id, "--prompt-file", str(prompt_file), *retry_options,
    ])
    create = next(value["receipt"] for value in retried if value.get("type") == "session_receipt")
    if create != session_receipt:
        raise RuntimeError(f"CLI chat retry created another session: {retried}")
    replayed = invoke(["session", "send", session_id, *retry_options], input_text=text)
    for frames in (retried, replayed):
        receipt = next(value["receipt"] for value in frames if value.get("type") == "receipt")
        terminal = next(value for value in frames if value.get("type") == "terminal")
        if (receipt != message_receipt or terminal["turn"]["status"] != "completed"
                or terminal["message"]["text"] != reply):
            raise RuntimeError(f"CLI did not reconcile the accepted turn on retry: {frames}")
    detached = invoke([
        "session", "send", session_id, "--prompt-file", str(prompt_file), "--detach", *retry_options,
    ])
    if detached != [{"type": "receipt", "receipt": message_receipt}]:
        raise RuntimeError(f"CLI detached send changed its receipt: {detached}")

    human = invoke([
        "chat", "--session", session_id, "--prompt", text, *retry_options,
    ], json_output=False)
    if human != reply + "\n":
        raise RuntimeError(f"CLI human turn following changed its canonical output: {human!r}")
    for command, expected in [
        (["models"], model_id),
        (["session", "list"], session_id),
        (["session", "show", session_id], "Revision: "),
        (["session", "history", session_id], "assistant: " + reply),
        (["turn", "show", turn_id], "completed"),
    ]:
        output = invoke(command, json_output=False)
        if expected not in output:
            raise RuntimeError(f"CLI human output for {command} is missing {expected!r}: {output!r}")

    if os.name == "posix":
        check_session_follow(prefix, env, root, session_id, message_receipt, reply)


def check_session_follow(prefix, env, root, session_id, message_receipt, reply):
    output_path = root / "cli-follow.ndjson"
    with output_path.open("w", encoding="utf-8") as output:
        follower = subprocess.Popen([
            *prefix, "--json", "session", "follow", session_id,
            "--after", str(message_receipt["event_sequence"]),
        ], env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                lines = output_path.read_text(encoding="utf-8").splitlines(keepends=True)
                frames = [json.loads(line) for line in lines if line.endswith("\n")]
                canonical = next((value for value in frames if value.get("type") == "canonical"), None)
                # Wait for the next open stream before signaling; terminal
                # reconciliation deliberately pauses between connections.
                if canonical is not None and frames[-1].get("type") == "heartbeat":
                    break
                if follower.poll() is not None:
                    raise RuntimeError("CLI session follower exited before canonical replay")
                time.sleep(0.05)
            else:
                raise RuntimeError("CLI session follower did not reconcile replayed history")
            if (canonical["turn"]["id"] != message_receipt["turn_id"]
                    or canonical["message"]["text"] != reply):
                raise RuntimeError(f"CLI session follow returned the wrong canonical reply: {canonical}")
            follower.send_signal(signal.SIGINT)
            _, diagnostics = follower.communicate(timeout=8)
            if follower.returncode != 0:
                raise RuntimeError(f"Ctrl-C did not detach the CLI follower cleanly: {diagnostics}")
        finally:
            if follower.poll() is None:
                follower.kill()
                follower.communicate(timeout=5)
