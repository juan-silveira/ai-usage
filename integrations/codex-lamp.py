#!/usr/bin/env python3
"""Turn on a lamp through a webhook when Codex asks a question."""

import argparse
from getpass import getpass
import json
import os
from pathlib import Path
import time
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

TEST_TIMEOUT_SECONDS = 180


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


# Connect directly to the loopback API and to the configured webhook.
HTTP = build_opener(ProxyHandler({}), NoRedirect())


def read_events(stream):
    name = None
    data = []
    for line in stream:
        line = line.decode("utf-8").rstrip("\r\n")
        if line.startswith("event:"):
            name = line[6:].strip()
        elif line.startswith("data:"):
            data.append(line[5:].lstrip(" "))
        elif not line:
            if name in ("sessions", "codex_question") and data:
                yield name, json.loads("\n".join(data))
            name, data = None, []


def pending_sessions(sessions):
    return {
        (session["session_id"], question["request_id"])
        for session in sessions
        if session["provider"] == "codex"
        for question in session.get("pending_questions", [])
    }


def turn_on(webhook):
    if webhook:
        with HTTP.open(Request(webhook, data=b"", method="POST"), timeout=5):
            pass
        print("Pedido para ligar a luminária enviado.", flush=True)
    else:
        print("Pergunta do Codex detectada.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=47821, help="Porta da API local do AI Usage")
    parser.add_argument("--dry-run", action="store_true", help="Exibe os eventos sem enviar o webhook")
    parser.add_argument("--once", action="store_true", help="Espera uma pergunta nova e encerra após o teste")
    parser.add_argument("--webhook-file", type=Path, help="Arquivo privado com a URL do webhook")
    args = parser.parse_args()
    webhook = None
    if not args.dry_run:
        webhook = os.environ.get("AI_USAGE_LAMP_WEBHOOK", "")
        if args.webhook_file is not None:
            try:
                webhook = args.webhook_file.read_text(encoding="utf-8").strip()
            except OSError:
                parser.error("Não foi possível ler o arquivo privado do webhook.")
        if not webhook and args.once:
            webhook = getpass("Cole a URL privada do webhook e pressione Enter: ").strip()
        url = urlsplit(webhook)
        if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password:
            parser.error("Configure AI_USAGE_LAMP_WEBHOOK com a URL do webhook.")

    endpoint = f"http://127.0.0.1:{args.port}/v1/events"
    delivered = set()
    ready = False
    retry_delay = 2
    deadline = time.monotonic() + TEST_TIMEOUT_SECONDS if args.once else float("inf")
    while time.monotonic() < deadline:
        try:
            with HTTP.open(endpoint, timeout=8 if args.once else 60) as stream:
                for event, data in read_events(stream):
                    if time.monotonic() >= deadline:
                        break
                    if event == "sessions":
                        pending = pending_sessions(data)
                        if not ready:
                            if args.once:
                                delivered.update(pending)
                            ready = True
                            message = "Pronto. Aguardando uma pergunta nova do Codex." if args.once else "Conectado à API local do AI Usage."
                            print(message, flush=True)
                        delivered.intersection_update(pending)
                    else:
                        pending = {(data["session_id"], data["request_id"])}
                    for question in pending - delivered:
                        turn_on(webhook)
                        delivered.add(question)
                        retry_delay = 2
                        if args.once:
                            return 0
        except TimeoutError:
            pass
        except (OSError, ValueError, KeyError) as exc:
            # Exceptions from urllib can include the secret webhook URL.
            print(f"Conexão interrompida ({type(exc).__name__}). Nova tentativa em {retry_delay}s.", flush=True)
            time.sleep(min(retry_delay, max(0, deadline - time.monotonic())))
            retry_delay = min(retry_delay * 2, 60)
            continue
        time.sleep(min(2, max(0, deadline - time.monotonic())))
    print("O teste terminou sem detectar uma pergunta nova.", flush=True)
    return 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        pass
